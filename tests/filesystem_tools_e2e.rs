//! Filesystem 工具端到端集成测试：ScriptedModel 驱动 write_file → read_file round-trip +
//! capability gating（FilesystemBackend 不支持 execute，模型看不到 execute 工具）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{Backend, DeepAgentBuilder, DeepAgentState, FilesystemBackend, FilesystemMiddleware};
use juncture::llm::{CallOptions, ChatModel, LlmError, Message, MessageChunk, Role, ToolDefinition};
use juncture::state::messages::ToolCall;
use juncture::RunnableConfig;
use serde_json::json;

/// 脚本模型：按序返回预设 turns（content + tool_calls），同时记录 bind_tools 收到的 tool 名。
#[derive(Clone)]
struct ScriptedModel {
    turns: Arc<Vec<(String, Vec<ToolCall>)>>,
    index: Arc<AtomicUsize>,
    seen_tools: Arc<Mutex<Vec<String>>>,
}

impl ScriptedModel {
    fn new(turns: Vec<(String, Vec<ToolCall>)>) -> Self {
        Self {
            turns: Arc::new(turns),
            index: Arc::new(AtomicUsize::new(0)),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn next_turn(&self) -> (String, Vec<ToolCall>) {
        let i = self.index.fetch_add(1, Ordering::Relaxed);
        self.turns.get(i).or_else(|| self.turns.last()).cloned().unwrap_or_default()
    }

    fn seen_tools(&self) -> Vec<String> {
        self.seen_tools.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ChatModel for ScriptedModel {
    async fn invoke(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        let (content, calls) = self.next_turn();
        Ok(Message::ai_with_tool_calls(&content, calls))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<juncture::llm::BoxStream<'_, Result<MessageChunk, LlmError>>, LlmError> {
        let chunk = MessageChunk {
            content: String::new(),
            tool_call_chunks: Vec::new(),
            usage_delta: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    fn bind_tools(&self, tools: Vec<ToolDefinition>) -> Self {
        let mut s = self.seen_tools.lock().expect("lock");
        s.clear();
        for t in &tools {
            s.push(t.name.clone());
        }
        self.clone()
    }

    fn model_name(&self) -> &str {
        "scripted"
    }
}

#[tokio::test]
async fn filesystem_tools_write_read_roundtrip() {
    let tmp = tempfile::tempdir().expect("tmp");
    let backend = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;

    // turn1: write_file(/out.txt, "hello world"); turn2: read_file(/out.txt); turn3: done
    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: json!({"file_path":"/out.txt","content":"hello world"}),
            }],
        ),
        (
            String::new(),
            vec![ToolCall {
                id: "c2".into(),
                name: "read_file".into(),
                arguments: json!({"file_path":"/out.txt"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model)
        .middleware_one(FilesystemMiddleware::new(backend))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("write hello world to out.txt then read it back")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 断言1：文件真实写出。
    let content = std::fs::read_to_string(tmp.path().join("out.txt")).expect("file exists");
    assert_eq!(content, "hello world");

    // 断言2：read_file 工具结果在 messages（Tool role，含 "hello world"）。
    let has_read_result = out
        .value
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains("hello world"));
    assert!(has_read_result, "read_file tool result should be in messages");

    // 断言3：最终 AI 消息 "done"（loop 终止）。
    let last_ai = out
        .value
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::Ai) && m.tool_calls.is_empty());
    assert!(last_ai.is_some(), "should have a final tool-call-free AI message");
}

#[tokio::test]
async fn filesystem_capability_gates_execute() {
    // FilesystemBackend 不是 SandboxBackend → execute 工具应被 capability 过滤掉。
    let tmp = tempfile::tempdir().expect("tmp");
    let backend = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;

    let model = ScriptedModel::new(vec![("done".into(), vec![])]);

    let agent = DeepAgentBuilder::new(model.clone())
        .middleware_one(FilesystemMiddleware::new(backend))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("hi")],
    };
    agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // FilesystemBackend.supported_tools() = 7（无 execute）。
    let seen = model.seen_tools();
    assert!(!seen.contains(&"execute".to_string()), "execute should be filtered: {seen:?}");
    assert!(seen.contains(&"read_file".to_string()), "read_file should be visible: {seen:?}");
    assert!(seen.contains(&"write_file".to_string()), "write_file should be visible: {seen:?}");
    assert_eq!(seen.len(), 7, "exactly 7 fs tools (no execute): {seen:?}");
}

#[tokio::test]
async fn local_shell_execute_tool_runs() {
    // LocalShellBackend 是 SandboxBackend → execute 工具可见 + 真实执行。
    use deepagents::LocalShellBackend;
    let backend = Arc::new(LocalShellBackend::with_cwd(
        std::env::temp_dir(),
    )) as Arc<dyn Backend>;

    // turn1: execute(echo hello); turn2: done. echo 跨平台行为一致（cmd /C echo + sh -c echo）。
    let echo_cmd = "echo hello";
    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "execute".into(),
                arguments: json!({"command":echo_cmd}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model)
        .middleware_one(FilesystemMiddleware::new(backend))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("run echo hello")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // execute 工具结果应含 "hello"。
    let has_echo = out
        .value
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains("hello"));
    assert!(has_echo, "execute output should contain 'hello'");
}

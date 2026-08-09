//! 默认栈端到端集成测试：验证 `DeepAgentBuilder::with_default_middleware()`
//! 套上的 always-on 栈（Summarization + PatchToolCalls + PromptCaching）在 ReAct loop
//! 里不破坏基本流程——既不误伤单轮纯文本回复，也不阻断正常的 tool_call → result → 终止 loop。
//!
//! 覆盖 `src/graph.rs` 的 `DeepAgentBuilder::with_default_middleware` + agent node
//! step 7 `remove_all` 语义，以及三个被测中间件（`summarization` / `patch_tool_calls`
//! / `prompt_caching`）在真实图执行里的 no-op / 不破坏行为。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{DeepAgentBuilder, DeepAgentState};
use juncture::RunnableConfig;
use juncture::llm::{
    CallOptions, ChatModel, LlmError, Message, MessageChunk, MockChatModel, Role, ToolDefinition,
};
use juncture::state::messages::ToolCall;
use juncture::tools::{Tool, ToolError};
use serde_json::{Value, json};

/// 脚本模型：按序返回预设 turns（content + tool_calls），同时记录 bind_tools 收到的 tool 名。
///
/// 复制自 `tests/filesystem_tools_e2e.rs`（integration test 是独立 crate，helper 不能跨文件复用）。
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
        self.turns
            .get(i)
            .or_else(|| self.turns.last())
            .cloned()
            .unwrap_or_default()
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

/// 简单回声工具：把入参 `text` 原样返回。用于验证 PatchToolCalls / 默认栈不误伤
/// 正常的 tool_call → ToolNode 执行 → Tool result → 终止 loop 流程。
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes back the text you provide."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The text to echo back." }
            },
            "required": ["text"]
        })
    }

    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
        Ok(text.to_string())
    }
}

#[tokio::test]
async fn default_stack_does_not_break_simple_reply() {
    // 默认栈（Summarization + PatchToolCalls + PromptCaching）+ 单轮纯文本回复：
    // 模型无 tool_call → 路由 END，loop 正常终止。默认栈中间件应全程 no-op
    // （消息数远低于 Summarization 阈值 50；末消息非 AI-with-toolcalls 故 PatchToolCalls
    // 不追加 synthetic；PromptCaching 恒 no-op）。
    let model = MockChatModel::new("gpt-4").with_response("done");

    let agent = DeepAgentBuilder::new(model)
        .with_default_middleware()
        .build()
        .expect("default-stack agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("hi")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("default-stack agent runs");

    // 断言1：invoke 成功（默认栈不破坏图编译 / 执行）。
    // 断言2：最终 AI message content_text == "done"（loop 末消息无 tool_calls，路由 END）。
    let last = out.value.messages.last().expect("has response message");
    assert!(
        matches!(last.role, Role::Ai),
        "last message should be AI: {:?}",
        last.role
    );
    assert!(
        last.tool_calls.is_empty(),
        "final AI message should have no tool_calls"
    );
    assert_eq!(last.content_text(), "done");
}

#[tokio::test]
async fn default_stack_preserves_normal_tool_call_loop() {
    // 默认栈 + 正常 tool_call loop：turn1 模型发 echo("hi")，turn2 终止。
    // 验证 PatchToolCallsMiddleware 不误伤正常 tool_call（before_agent 仅修末尾 dangling
    // ——turn2 进入 agent node 时末消息是 Tool result 而非 AI-with-toolcalls，不追加 synthetic），
    // Summarization 不折叠（消息数 < 50），PromptCaching no-op。EchoTool 结果应出现在
    // messages，且 loop 正常以 "done" 终止。
    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: json!({"text":"hi"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model.clone())
        .with_default_middleware()
        .tool(Box::new(EchoTool))
        .build()
        .expect("default-stack agent with echo builds");

    let state = DeepAgentState {
        messages: vec![Message::human("echo hi back to me")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("default-stack agent with echo runs");

    // 断言1：模型 bind_tools 收到 echo（默认栈中间件不提供工具，caller 的 echo 透传）。
    let seen = model.seen_tools();
    assert!(
        seen.contains(&"echo".to_string()),
        "echo tool should be visible to model: {seen:?}"
    );

    // 断言2：EchoTool 的结果（"hi"）作为 Tool-role 消息出现在 messages。
    let has_echo_result = out
        .value
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains("hi"));
    assert!(
        has_echo_result,
        "echo tool result should be in messages: {:?}",
        out.value
            .messages
            .iter()
            .map(|m| (format!("{:?}", m.role), m.content_text().to_string()))
            .collect::<Vec<_>>()
    );

    // 断言3：最终 AI 消息 "done" 且无 tool_calls（loop 正常终止，未被默认栈干预成死循环）。
    let last = out.value.messages.last().expect("has final message");
    assert!(
        matches!(last.role, Role::Ai),
        "final message should be AI: {:?}",
        last.role
    );
    assert!(
        last.tool_calls.is_empty(),
        "final AI message should have no tool_calls"
    );
    assert_eq!(last.content_text(), "done");
}

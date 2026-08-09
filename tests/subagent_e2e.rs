//! Subagent 端到端集成测试：验证 `SubagentMiddleware` 在 ReAct loop 里真正生效——
//! task 工具调用预编译子 agent + 隔离上下文（全新 messages，不继承 caller state）
//! + 返回子 agent 最终回复。
//!
//! 复刻 deepagents `SubAgentMiddleware` 语义：子 agent 由调用方用 `create_deep_agent`
//! 预编译后注册进 `AgentRegistry`；`TaskTool.invoke` 起全新 state（仅一条
//! `Message::human(&task)`）调用子 agent，从末尾向前找首条无 tool_calls 的 AI 消息返回。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{
    AgentRegistry, DeepAgentBuilder, DeepAgentConfig, DeepAgentState, SubagentMiddleware,
    create_deep_agent,
};
use juncture::RunnableConfig;
use juncture::graph::CompiledGraph;
use juncture::llm::{
    CallOptions, ChatModel, LlmError, Message, MessageChunk, MockChatModel, Role, ToolDefinition,
};
use juncture::state::messages::ToolCall;
use serde_json::json;

/// 脚本模型（复制自 `tests/filesystem_tools_e2e.rs`，扩展记录首条 system message）：
/// 按序返回预设 turns（content + tool_calls），同时记录 bind_tools 收到的 tool 名 +
/// invoke 收到的首条 system message（用于断言 SubagentMiddleware 注入的可用子 agent 段）。
#[derive(Clone)]
struct ScriptedModel {
    turns: Arc<Vec<(String, Vec<ToolCall>)>>,
    index: Arc<AtomicUsize>,
    seen_tools: Arc<Mutex<Vec<String>>>,
    seen_system: Arc<Mutex<String>>,
}

impl ScriptedModel {
    fn new(turns: Vec<(String, Vec<ToolCall>)>) -> Self {
        Self {
            turns: Arc::new(turns),
            index: Arc::new(AtomicUsize::new(0)),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_system: Arc::new(Mutex::new(String::new())),
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

    fn seen_system(&self) -> String {
        self.seen_system.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ChatModel for ScriptedModel {
    async fn invoke(
        &self,
        messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        // 记录首条 system message（SubagentMiddleware 注入的 "Available sub-agents" 段）。
        if let Some(m) = messages.first()
            && matches!(m.role, Role::System)
        {
            *self.seen_system.lock().expect("lock") = m.content_text().to_string();
        }
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

/// 记录模型：返回固定回复，同时记录 invoke 收到的 messages（role + content_text）。
/// 用作子 agent 的模型，以验证 `TaskTool` 起全新 state 的隔离语义——
/// 子 agent 不应看到 caller 的对话历史。
#[derive(Clone)]
struct RecordingModel {
    response: String,
    seen_messages: Arc<Mutex<Vec<(Role, String)>>>,
}

impl RecordingModel {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            seen_messages: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn seen_messages(&self) -> Vec<(Role, String)> {
        self.seen_messages.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ChatModel for RecordingModel {
    async fn invoke(
        &self,
        messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        let mut rec = self.seen_messages.lock().expect("lock");
        rec.clear();
        for m in messages {
            rec.push((m.role.clone(), m.content_text().to_string()));
        }
        drop(rec);
        // 返回无 tool_calls 的 AI 消息 → 子 agent 路由 END，终止 ReAct loop。
        Ok(Message::ai(&self.response))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<juncture::llm::BoxStream<'_, Result<MessageChunk, LlmError>>, LlmError> {
        let chunk = MessageChunk {
            content: self.response.clone(),
            tool_call_chunks: Vec::new(),
            usage_delta: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    fn bind_tools(&self, _tools: Vec<ToolDefinition>) -> Self {
        // 子 agent 无工具：bind_tools 收到空集，返回共享 seen_messages 的 clone。
        self.clone()
    }

    fn model_name(&self) -> &str {
        "recording"
    }
}

/// 构造注册表：`{"worker" => 预编译子 agent}`。子 agent 无工具、无中间件，
/// 单轮即终止（MockChatModel / RecordingModel 返回无 tool_calls 的 AI 消息）。
fn worker_registry(child: CompiledGraph<DeepAgentState>) -> AgentRegistry {
    let mut registry: HashMap<String, Arc<CompiledGraph<DeepAgentState>>> = HashMap::new();
    registry.insert("worker".to_string(), Arc::new(child));
    Arc::new(registry)
}

#[tokio::test]
async fn subagent_delegates_task_and_returns_subagent_response() {
    // 子 agent：MockChatModel 返回 "4"，无工具——用 create_deep_agent 预编译。
    let child_model = MockChatModel::new("worker-model").with_response("4");
    let child_agent = create_deep_agent(DeepAgentConfig::new(child_model)).expect("child compiles");
    let registry = worker_registry(child_agent);

    // 主 agent turn1: task({subagent_type:"worker", task:"compute 2+2"}); turn2: done。
    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "task".into(),
                arguments: json!({"subagent_type":"worker","task":"compute 2+2"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model.clone())
        .middleware_one(SubagentMiddleware::new(registry))
        .build()
        .expect("main agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("delegate compute to worker")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 断言1：task 工具结果在 messages（Tool role），其 content_text 含子 agent 回复 "4"。
    let has_task_result = out
        .value
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains('4'));
    assert!(
        has_task_result,
        "task tool result should contain subagent response '4'"
    );

    // 断言2：主 agent 看到可用子 agent 列表注入 system_message。
    let sys = model.seen_system();
    assert!(
        sys.contains("Available sub-agents"),
        "system should list available subagents: {sys}"
    );
    assert!(
        sys.contains("worker"),
        "system should name the worker subagent: {sys}"
    );

    // 断言3：task 工具对主 agent 可见（bind_tools 收到 "task"）。
    let seen = model.seen_tools();
    assert!(
        seen.contains(&"task".to_string()),
        "task tool should be bound to main agent: {seen:?}"
    );

    // 断言4：最终 AI 消息 "done"（无 tool_calls，loop 终止）。
    let last_ai = out
        .value
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::Ai) && m.tool_calls.is_empty());
    assert!(
        last_ai.is_some(),
        "should have a final tool-call-free AI message"
    );
}

#[tokio::test]
async fn subagent_isolates_child_context() {
    // 子 agent：RecordingModel 记录收到的 messages，返回 "4"。
    // 用以验证 TaskTool 起全新 state——子 agent 只看到 task 文本，不继承 caller 历史。
    let child_model = RecordingModel::new("4");
    let child_agent =
        create_deep_agent(DeepAgentConfig::new(child_model.clone())).expect("child compiles");
    let registry = worker_registry(child_agent);

    // 主 agent turn1: task({subagent_type:"worker", task:"compute 2+2"}); turn2: done。
    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "task".into(),
                arguments: json!({"subagent_type":"worker","task":"compute 2+2"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model)
        .middleware_one(SubagentMiddleware::new(registry))
        .build()
        .expect("main agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("delegate compute to worker")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 断言1：子 agent 只收到 1 条消息（fresh state 的 human task），非 caller 全历史。
    let child_seen = child_model.seen_messages();
    assert_eq!(
        child_seen.len(),
        1,
        "child should see exactly the fresh task message (isolation), got: {child_seen:?}"
    );
    assert_eq!(
        child_seen[0].0,
        Role::Human,
        "child's sole message should be Human role"
    );
    assert!(
        child_seen[0].1.contains("compute 2+2"),
        "child's sole message should be the task text: {}",
        child_seen[0].1
    );

    // 断言2：task 工具结果含子 agent 回复 "4"（隔离不破坏回复回传）。
    let has_task_result = out
        .value
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains('4'));
    assert!(
        has_task_result,
        "task tool result should still contain subagent response '4'"
    );
}

#[tokio::test]
async fn subagent_unknown_type_returns_error() {
    // 空注册表：task 调用未知类型 → "Error: unknown subagent: worker"。
    // （空注册表时 wrap_model_call 不注入 prompt，但 TaskTool 仍暴露 + 报错。）
    let registry: AgentRegistry = Arc::new(HashMap::new());

    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "task".into(),
                arguments: json!({"subagent_type":"worker","task":"compute 2+2"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model)
        .middleware_one(SubagentMiddleware::new(registry))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("delegate to unknown")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 断言：task 工具结果含 "Error: unknown subagent"（对齐 deepagents 未知类型报错约定）。
    let has_err = out.value.messages.iter().any(|m| {
        matches!(m.role, Role::Tool) && m.content_text().contains("Error: unknown subagent")
    });
    assert!(
        has_err,
        "unknown subagent type should yield 'Error: unknown subagent' tool result"
    );
}

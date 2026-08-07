//! MVP 拦截语义集成测试：验证 `FilesystemMiddleware` 的 `wrap_model_call`
//! 过滤不支持的工具 + 注入 system prompt 段。这是 B 路径拦截语义的核心证据。
//!
//! 断言：
//! - 模型 `bind_tools` 收到的 tools 仅 `read_file`（`write_file` 被按后端能力过滤）
//! - 模型 `invoke` 收到的 system message 含 Filesystem 指导段 + 用户 prompt

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{DeepAgentBuilder, DeepAgentState, FilesystemMiddleware, ReadonlyBackend};
use juncture::llm::{
    CallOptions, ChatModel, LlmError, Message, MessageChunk, Role, ToolDefinition,
};
use juncture::tools::ToolError;
use juncture::tools::Tool;
use juncture::RunnableConfig;
use serde_json::json;

/// 间谍模型：记录 `bind_tools` 收到的 tool 名 + `invoke` 收到的 system message。
#[derive(Clone)]
struct SpyModel {
    response: String,
    seen_tools: Arc<Mutex<Vec<String>>>,
    seen_system: Arc<Mutex<String>>,
}

impl SpyModel {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_system: Arc::new(Mutex::new(String::new())),
        }
    }

    fn seen_tools(&self) -> Vec<String> {
        self.seen_tools.lock().expect("lock").clone()
    }

    fn seen_system(&self) -> String {
        self.seen_system.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ChatModel for SpyModel {
    async fn invoke(
        &self,
        messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        // 记录首条 system message（agent node 在 messages[0] prepend system）。
        if let Some(m) = messages.first()
            && matches!(m.role, Role::System)
        {
            *self.seen_system.lock().expect("lock") = m.content_text().to_string();
        }
        // 返回无 tool_calls 的 AI 消息 → 路由 END，终止 ReAct loop。
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

    fn bind_tools(&self, tools: Vec<ToolDefinition>) -> Self {
        let mut s = self.seen_tools.lock().expect("lock");
        s.clear();
        for t in &tools {
            s.push(t.name.clone());
        }
        self.clone()
    }

    fn model_name(&self) -> &str {
        "spy"
    }
}

struct ReadFileTool;
#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a file's contents."
    }
    fn schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
    }
    async fn invoke(&self, _input: serde_json::Value) -> Result<String, ToolError> {
        Ok("file content".to_string())
    }
}

struct WriteFileTool;
#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Write content to a file."
    }
    fn schema(&self) -> serde_json::Value {
        json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]})
    }
    async fn invoke(&self, _input: serde_json::Value) -> Result<String, ToolError> {
        Ok("wrote".to_string())
    }
}

#[tokio::test]
async fn filesystem_filters_unsupported_and_injects_prompt() {
    let spy = SpyModel::new("done");
    let agent = DeepAgentBuilder::new(spy.clone())
        .tool(Box::new(ReadFileTool))
        .tool(Box::new(WriteFileTool))
        .system_prompt("You are a coding agent.")
        .middleware_one(FilesystemMiddleware::new(Arc::new(ReadonlyBackend)))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("read foo.txt please")],
    };
    agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 断言1：write_file 被 FilesystemMiddleware 按后端能力过滤，模型只看到 read_file。
    assert_eq!(spy.seen_tools(), vec!["read_file".to_string()]);

    // 断言2：system message 含用户 prompt + Filesystem 指导段（被中间件注入）。
    let sys = spy.seen_system();
    assert!(
        sys.contains("You are a coding agent."),
        "system preserves user prompt: {sys}"
    );
    assert!(sys.contains("Filesystem"), "system has fs prose: {sys}");
}

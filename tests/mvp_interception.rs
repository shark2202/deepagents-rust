//! 拦截语义集成测试：验证 `FilesystemMiddleware` 提供工具 + per-call capability 过滤 + prompt 注入。
//!
//! 增量 B 后语义升级：FilesystemMiddleware 自己提供 8 个 fs 工具（持 backend），
//! caller 不再重复传。测试验证：ReadonlyBackend（只支持 read_file）时，
//! 8 工具经 wrap_model_call 过滤后模型只看到 read_file。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{DeepAgentBuilder, DeepAgentState, FilesystemMiddleware, ReadonlyBackend};
use juncture::RunnableConfig;
use juncture::llm::{
    CallOptions, ChatModel, LlmError, Message, MessageChunk, Role, ToolDefinition,
};

/// 间谍模型：记录 bind_tools 收到的 tool 名 + invoke 收到的 system message。
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

#[tokio::test]
async fn filesystem_filters_unsupported_and_injects_prompt() {
    let spy = SpyModel::new("done");
    // FilesystemMiddleware 提供 8 个 fs 工具；ReadonlyBackend 只支持 read_file。
    // 无 caller tools —— 工具全由中间件提供。
    let agent = DeepAgentBuilder::new(spy.clone())
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

    // 断言1：8 工具中只 read_file 通过 capability 过滤（ReadonlyBackend.supported_tools=["read_file"]）。
    assert_eq!(spy.seen_tools(), vec!["read_file".to_string()]);

    // 断言2：system message 含用户 prompt + Filesystem 指导段。
    let sys = spy.seen_system();
    assert!(
        sys.contains("You are a coding agent."),
        "system preserves user prompt: {sys}"
    );
    assert!(sys.contains("Filesystem"), "system has fs prose: {sys}");
}

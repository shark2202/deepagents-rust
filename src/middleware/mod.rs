//! 中间件拦截层 —— B 路径的核心。
//!
//! 对应 deepagents `AgentMiddleware.wrap_model_call(request, handler)`。每个 deepagents 特性
//! （Filesystem/Subagent/Skills/Memory/Summarization/PatchToolCalls/PromptCaching）都是一个
//! `Middleware` 实现，在每次 LLM 调用前 mute `ModelRequest`（过滤 tools / 注入 system 段 /
//! 改 messages），并可读写 `DeepAgentState`。
//!
//! # 与 juncture `AgentMiddleware` 的区别
//!
//! juncture 的 `before_model(&state)` 只读 state、tools 在 `model.bind_tools()` 预绑死、
//! `CallOptions` 无 `tools` 字段——无法承载 deepagents 的 per-call tool 过滤等拦截语义，
//! 故自建本 trait。juncture runtime（`StateGraph`/Pregel/`Command`/`Node`）仍照常使用。

pub mod filesystem;

use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use juncture::llm::{CallOptions, Message, ToolDefinition};

use crate::state::DeepAgentState;

/// 每次 LLM 调用前由 agent node 构造的请求；中间件按链序 mutate 各字段。
///
/// 注意：`messages` **不在**此处——永远取 `state.messages` 最新值。中间件若需
/// 截断/驱逐/移除消息（如 `SummarizationMiddleware`），直接改 `state.messages`。
pub struct ModelRequest {
    /// 本次调用暴露给模型工具定义列表。中间件可过滤（如 `FilesystemMiddleware`
    /// 按后端能力裁剪不让后端支持的 tool）。
    pub tools: Vec<ToolDefinition>,
    /// 系统提示。中间件可 `push_str` 追加段（Filesystem 用法 prose / Skills 索引 /
    /// Memory `<agent_memory>` / Subagent 可用列表）。初始值为调用方 `system_prompt`。
    pub system_message: String,
    /// 本次调用的 per-call 选项（temperature / tool_choice / model_override 等）。
    pub options: CallOptions,
}

impl Debug for ModelRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRequest")
            .field("tools", &self.tools.len())
            .field("system_message_len", &self.system_message.len())
            .finish_non_exhaustive()
    }
}

/// 中间件错误。
#[derive(Debug, thiserror::Error)]
pub enum MiddlewareError {
    /// 中间件执行失败。
    #[error("middleware error: {0}")]
    Other(String),
}

/// DeepAgents 中间件 trait —— 对应 deepagents `AgentMiddleware`。
///
/// 所有 hook 默认 no-op；实现者按需覆写。链序执行（`before_*`/`wrap` 正向，`after_*` 反向）。
#[async_trait]
pub trait Middleware: Send + Sync + Debug {
    /// agent run 开始：mutate state。用于 `PatchToolCallsMiddleware`（修 dangling tool calls）等。
    async fn before_agent(&self, _state: &mut DeepAgentState) -> Result<(), MiddlewareError> {
        Ok(())
    }

    /// 每次 LLM 调用前：mutate `req`（tools/system/options）+ 可改 `state`（私有字段 / messages 截断）。
    /// 这是 deepagents 拦截语义的主 hook。
    async fn wrap_model_call(
        &self,
        _req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        Ok(())
    }

    /// 每次 LLM 调用后：处理 `response` + 可改 `state`。替代 deepagents 的
    /// `ExtendedModelResponse`（响应 + `Command`）——直接 mutate state 更 Rust 惯用。
    async fn after_model_call(
        &self,
        _response: &mut Message,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        Ok(())
    }
}

/// 有序中间件链。
///
/// `before_agent` / `wrap_model_call` 按插入序正向执行；`after_model_call` 按逆序执行
/// （与 juncture `AgentMiddlewareChain` 一致，洋葱模型）。
#[derive(Default, Clone)]
pub struct MiddlewareChain {
    inner: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareChain {
    /// 空链。
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    /// 追加一个中间件。
    #[must_use]
    pub fn with(mut self, m: impl Middleware + 'static) -> Self {
        self.inner.push(Arc::new(m));
        self
    }

    /// 链长。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 是否空链。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 正向跑 `before_agent`。
    pub async fn before_agent(&self, state: &mut DeepAgentState) -> Result<(), MiddlewareError> {
        for m in &self.inner {
            m.before_agent(state).await?;
        }
        Ok(())
    }

    /// 正向跑 `wrap_model_call`。
    pub async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        for m in &self.inner {
            m.wrap_model_call(req, state).await?;
        }
        Ok(())
    }

    /// 逆序跑 `after_model_call`。
    pub async fn after_model_call(
        &self,
        response: &mut Message,
        state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        for m in self.inner.iter().rev() {
            m.after_model_call(response, state).await?;
        }
        Ok(())
    }
}

impl Debug for MiddlewareChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MiddlewareChain")
            .field("len", &self.inner.len())
            .finish()
    }
}

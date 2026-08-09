//! `deepagents` —— LangChain "Deep Agents" SDK harness 的 Rust 移植，建在 juncture（LangGraph runtime）之上。
//!
//! # 架构（B 路径）
//!
//! juncture 提供完整 LangGraph 底层原语（`StateGraph`/Pregel/`Command`/`interrupt!`/`Store`/`Checkpointer`/`ChatModel`/`Tool`），
//! 但缺少 deepagents 的**中间件拦截层**（每次 LLM 调用前重写 tools/messages/system）。
//! 本 crate 自建该层（`DeepAgentNode` + `Middleware` trait），与 deepagents 自身在 langchain
//! `create_agent` 之上补 `create_deep_agent` 同构。
//!
//! # 中间件契约（3-hook 语义 C）
//!
//! - `before_agent(&mut State)` —— agent run 开始（修 dangling tool calls 等）
//! - `wrap_model_call(&mut ModelRequest, &mut State)` —— 每次 LLM 调用前过滤 tools / 注入 system / 改 messages
//! - `after_model_call(&mut Message, &mut State)` —— 处理 response / 写 state
//!
//! `ModelRequest = { tools, system_message, options }`；`messages` 永远来自 `state.messages`（中间件改 state 即持久化）。

pub mod backend;
pub mod graph;
pub mod middleware;
pub mod permission;
pub mod profiles;
pub mod state;

pub use backend::{
    Backend, CompositeBackend, FilesystemBackend, LocalShellBackend, ReadonlyBackend,
    SandboxBackend,
};
pub use graph::{DeepAgentBuilder, DeepAgentConfig, create_deep_agent};
pub use middleware::async_subagent::AsyncSubAgentMiddleware;
pub use middleware::filesystem::FilesystemMiddleware;
pub use middleware::memory::MemoryMiddleware;
pub use middleware::patch_tool_calls::PatchToolCallsMiddleware;
pub use middleware::prompt_caching::AnthropicPromptCachingMiddleware;
pub use middleware::skills::{SkillMetadata, SkillsMiddleware};
pub use middleware::subagent::{AgentRegistry, SubagentMiddleware};
pub use middleware::summarization::SummarizationMiddleware;
pub use middleware::{Middleware, MiddlewareChain, MiddlewareError, ModelRequest};
pub use permission::{
    FilesystemOperation, FilesystemPermission, PermissionMode, check_fs_permission,
};
pub use profiles::{HarnessProfile, ProfileRegistry, apply_profile_prompt};
pub use state::DeepAgentState;

//! `FilesystemMiddleware` —— 提供 8 个 fs 工具 + per-call capability 过滤 + system prompt 注入。
//!
//! 对应 deepagents `FilesystemMiddleware`：
//! - `tools()`：返回 8 个 fs 工具（持 backend），与 caller tools 加性合并进 agent 工具集
//! - `wrap_model_call`：按 `backend.supported_tools()` per-call 过滤可见性（capability gating，
//!   对齐 deepagents `_filter_unsupported_tools_and_apply_prompt`）+ 注入 fs 用法 prose
//!
//! 完整版（增量 C）追加：`FilesystemPermission` 穿插（deny 在工具层 / interrupt 交 HITL）、
//! 大消息驱逐、CompositeBackend host-path 路由 prompt。

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use juncture::tools::Tool;

use crate::backend::Backend;
use crate::middleware::fs_tools::all_fs_tools;
use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 文件系统中间件：提供 8 工具 + per-call capability 过滤 + prompt 注入。
pub struct FilesystemMiddleware {
    backend: Arc<dyn Backend>,
}

impl std::fmt::Debug for FilesystemMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesystemMiddleware")
            .field("backend", &self.backend)
            .finish()
    }
}

impl FilesystemMiddleware {
    /// 以给定后端构造。
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }

    /// 后端引用（供外部构造 backend-aware 工具或 CompositeBackend）。
    #[must_use]
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }
}

#[async_trait]
impl Middleware for FilesystemMiddleware {
    /// 提供 8 个 fs 工具（持 backend clone）。构造顺序对齐 deepagents。
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        all_fs_tools(Arc::clone(&self.backend))
    }

    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // 1. capability gating：按 backend.supported_tools() 过滤可见工具。
        //    execute 需 SandboxBackend（supported_tools 含 execute）；delete 需 backend override；
        //    其余 6 恒暴露（由 backend.supported_tools() 列出）。
        let supported: HashSet<&str> = self.backend.supported_tools().iter().copied().collect();
        req.tools.retain(|t| supported.contains(t.name.as_str()));

        // 2. 注入 fs 用法 prose（多中间件 push_str 叠加段）。
        req.system_message.push_str(
            "\n\nFilesystem: use read_file to inspect files. Available tools are filtered by backend capability.",
        );

        Ok(())
    }
}


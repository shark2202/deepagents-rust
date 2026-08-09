//! `FilesystemMiddleware` —— 提供 8 个 fs 工具 + per-call capability 过滤 + system prompt 注入
//! + 携带 `FilesystemPermission`（工具层 deny enforce，interrupt 交 HITL defer）。
//!
//! 对应 deepagents `FilesystemMiddleware`：
//! - `tools()`：返回 8 个 fs 工具（持 backend + permissions），与 caller tools 加性合并
//! - `wrap_model_call`：按 `backend.supported_tools()` per-call 过滤可见性 + 注入 fs 用法 prose
//! - permissions：工具 invoke 内 `check_deny`（Deny → "Error: permission denied..."）

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use juncture::tools::Tool;

use crate::backend::Backend;
use crate::middleware::fs_tools::all_fs_tools;
use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::permission::FilesystemPermission;
use crate::state::DeepAgentState;

/// 文件系统中间件：提供 8 工具 + per-call capability 过滤 + prompt 注入 + 携带权限规则。
pub struct FilesystemMiddleware {
    backend: Arc<dyn Backend>,
    permissions: Arc<[FilesystemPermission]>,
}

impl std::fmt::Debug for FilesystemMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesystemMiddleware")
            .field("backend", &self.backend)
            .field("permissions_count", &self.permissions.len())
            .finish()
    }
}

impl FilesystemMiddleware {
    /// 以给定后端构造（无权限规则）。
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            permissions: Arc::from([]),
        }
    }

    /// 以给定后端 + 权限规则构造。
    #[must_use]
    pub fn with_permissions(
        backend: Arc<dyn Backend>,
        permissions: Vec<FilesystemPermission>,
    ) -> Self {
        Self {
            backend,
            permissions: Arc::from(permissions),
        }
    }

    /// 后端引用（供外部构造 CompositeBackend 等）。
    #[must_use]
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }
}

#[async_trait]
impl Middleware for FilesystemMiddleware {
    /// 提供 8 个 fs 工具（持 backend + permissions clone）。构造顺序对齐 deepagents。
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        all_fs_tools(Arc::clone(&self.backend), Arc::clone(&self.permissions))
    }

    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // 1. capability gating：按 backend.supported_tools() 过滤可见工具。
        let supported: HashSet<&str> = self.backend.supported_tools().iter().copied().collect();
        req.tools.retain(|t| supported.contains(t.name.as_str()));

        // 2. 注入 fs 用法 prose（多中间件 push_str 叠加段）。
        req.system_message.push_str(
            "\n\nFilesystem: use read_file to inspect files. Available tools are filtered by backend capability.",
        );

        Ok(())
    }
}

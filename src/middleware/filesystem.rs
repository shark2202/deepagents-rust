//! `FilesystemMiddleware`（MVP 最小版）。
//!
//! 对应 deepagents `FilesystemMiddleware.wrap_model_call`：
//! 1. 按后端能力过滤不支持的工具（[deepagents] `_filter_unsupported_tools_and_apply_prompt`）
//! 2. 注入文件系统用法 prose 到系统提示
//!
//! 完整版（Phase 2）追加：read/write/edit/grep/glob/ls/delete/execute 工具实现、
//! 大消息驱逐到 backend（`_evict_and_truncate_messages`）、多模态 scrub、CompositeBackend
//! 路径路由、`state.messages` 持久化截断。MVP 仅验证拦截语义成立。

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::Backend;
use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 文件系统中间件。MVP：per-call tool 过滤 + system prompt 注入。
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
}

#[async_trait]
impl Middleware for FilesystemMiddleware {
    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // 1. 按 backend 能力过滤：仅保留 supported_tools 内的工具定义。
        let supported: HashSet<&str> = self.backend.supported_tools().iter().copied().collect();
        req.tools.retain(|t| supported.contains(t.name.as_str()));

        // 2. 注入 fs 用法 prose 到系统提示（多中间件 push_str 叠加段）。
        req.system_message.push_str(
            "\n\nFilesystem: use read_file to inspect files. Available tools are filtered by backend capability.",
        );

        Ok(())
    }
}

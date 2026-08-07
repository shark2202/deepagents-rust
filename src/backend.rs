//! 文件后端抽象（MVP 最小版）。
//!
//! Phase 2 会扩展为完整 deepagents `BackendProtocol`（read/write/edit/grep/glob/ls/execute
//! + `SandboxBackendProtocol`）及 State/Store/Filesystem/Composite 实现。
//!
//! MVP 只需能力表达以演示 per-call tool 过滤——这是 B 路径拦截语义的核心证据。

use std::fmt::Debug;

/// 文件后端。MVP：只表达支持的工具集合（用于 [`crate::middleware::filesystem::FilesystemMiddleware`] 的 per-call 过滤）。
pub trait Backend: Send + Sync + Debug {
    /// 该 backend 支持的工具名（白名单）。中间件据此从 `ModelRequest.tools` 裁剪不支持的工具。
    fn supported_tools(&self) -> &'static [&'static str];
}

/// 只读后端（MVP）：仅支持 `read_file`。演示拦截语义：`write_file` 等会被中间件过滤掉。
#[derive(Debug, Default)]
pub struct ReadonlyBackend;

impl Backend for ReadonlyBackend {
    fn supported_tools(&self) -> &'static [&'static str] {
        &["read_file"]
    }
}

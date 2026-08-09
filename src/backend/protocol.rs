//! `Backend` / `SandboxBackend` trait —— 对应 deepagents `BackendProtocol` / `SandboxBackendProtocol`。
//!
//! 默认方法返回 not-implemented，backend 按需 override。`as_sandbox()` 用于 capability gating
//! （`execute` 工具仅当 backend 是 `SandboxBackend` 时暴露）。

use async_trait::async_trait;
use std::fmt::Debug;

use super::types::*;

/// 文件后端协议。所有 backend 实现此 trait。文件操作默认 not-implemented。
#[async_trait]
pub trait Backend: Send + Sync + Debug {
    /// 该 backend 暴露给 LLM 的工具名（用于 FilesystemMiddleware 的 per-call 过滤）。
    /// 默认全 8 个恒暴露；具体 backend 可 override 裁剪（如 ReadonlyBackend 只 `read_file`）。
    /// 返回 `Vec` 而非 `&'static` 以支持 `CompositeBackend` 动态 union。
    fn supported_tools(&self) -> Vec<&'static str> {
        vec!["ls", "read_file", "write_file", "edit_file", "delete", "glob", "grep", "execute"]
    }

    /// 列目录。
    async fn ls(&self, _path: &str) -> LsResult { LsResult::err("not implemented: ls") }
    /// 读文件行窗口。
    async fn read(&self, _file_path: &str, _offset: usize, _limit: usize) -> ReadResult { ReadResult::err("not implemented: read") }
    /// 字面量文本搜索（非 regex）。
    async fn grep(&self, _pattern: &str, _path: Option<&str>, _glob: Option<&str>, _max_count: Option<usize>) -> GrepResult { GrepResult::err("not implemented: grep") }
    /// glob 匹配文件。
    async fn glob(&self, _pattern: &str, _path: Option<&str>) -> GlobResult { GlobResult::err("not implemented: glob") }
    /// 写文件（创建或覆盖）。
    async fn write(&self, _file_path: &str, _content: &str) -> WriteResult { WriteResult::err("not implemented: write") }
    /// 精确字符串替换。
    async fn edit(&self, _file_path: &str, _old_string: &str, _new_string: &str, _replace_all: bool) -> EditResult { EditResult::err("not implemented: edit") }
    /// 递归删除。
    async fn delete(&self, _file_path: &str) -> DeleteResult { DeleteResult::err("not implemented: delete") }

    /// 若 backend 支持沙箱执行则返回引用。capability gating 用（`execute` 工具据此暴露）。
    fn as_sandbox(&self) -> Option<&dyn SandboxBackend> { None }
}

/// 沙箱后端协议：扩展 `Backend` 加 shell 执行。
#[async_trait]
pub trait SandboxBackend: Backend {
    /// sandbox 实例 id。
    fn id(&self) -> &str;
    /// 执行 shell 命令。timeout=None 用默认；0 表示不限制（支持的后端）。
    async fn execute(&self, _command: &str, _timeout: Option<u32>) -> ExecuteResponse {
        ExecuteResponse::new("not implemented: execute", None)
    }
}

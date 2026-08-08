//! 文件后端抽象 —— 对应 deepagents `BackendProtocol`。
//!
//! backends 在不同位置存储文件（本地磁盘、state、store、远程），提供统一文件操作接口。
//! `Backend` 是基础（ls/read/grep/glob/write/edit/delete），`SandboxBackend` 扩展 `execute`。
//!
//! Phase 2a 增量 A：trait + 数据类 + `FilesystemBackend`(本地) + `LocalShellBackend`(subprocess)。
//! `ReadonlyBackend` 保留以维持 MVP 测试。

mod filesystem;
mod local_shell;
mod protocol;
mod readonly;
mod types;

pub use filesystem::FilesystemBackend;
pub use local_shell::LocalShellBackend;
pub use protocol::{Backend, SandboxBackend};
pub use readonly::ReadonlyBackend;
pub use types::*;

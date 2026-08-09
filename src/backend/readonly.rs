//! `ReadonlyBackend` —— MVP 用，演示 per-call tool 过滤。
//!
//! 只暴露 `read_file`（`supported_tools` override），其余继承 default not-implemented。
//! 不做真实 I/O——MVP 集成测试只需过滤语义成立。

use async_trait::async_trait;

use super::protocol::Backend;
use super::types::*;

/// 只读后端（MVP）：仅暴露 `read_file`。
#[derive(Debug, Default)]
pub struct ReadonlyBackend;

#[async_trait]
impl Backend for ReadonlyBackend {
    fn supported_tools(&self) -> Vec<&'static str> {
        vec!["read_file"]
    }

    async fn read(&self, _file_path: &str, _offset: usize, _limit: usize) -> ReadResult {
        // MVP：不做真实 I/O。真实场景用 FilesystemBackend。
        ReadResult::err("ReadonlyBackend: no files configured")
    }
}

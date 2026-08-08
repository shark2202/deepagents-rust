//! 文件操作结果数据类 —— 对应 deepagents `backends/protocol.py` 的 dataclass。
//!
//! 行为对齐：`error: Option<String>`（None=成功）、`truncated`、`entries/matches: Option<Vec<...>>`。
//! `ReadResult` 的 pagination 不变量（start_line/end_line 成对、next_offset==end_line 等）由
//! backend 实现负责保证；Rust 不强制 `__post_init__`（构造便宜，校验在单测）。

use serde::{Deserialize, Serialize};

/// 目录条目信息。只有 `path` 必需，其余 best-effort。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileInfo {
    /// 绝对或相对路径。
    pub path: String,
    /// 是否目录。
    pub is_dir: Option<bool>,
    /// 字节数（近似）。
    pub size: Option<u64>,
    /// ISO 8601 修改时间。
    pub modified_at: Option<String>,
}

/// grep 上下文行（非匹配的相邻行）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContextLine {
    /// 1-indexed 行号。
    pub line: usize,
    /// 行内容。
    pub text: String,
}

/// 单个 grep 匹配。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GrepMatch {
    /// 文件路径。
    pub path: String,
    /// 1-indexed 行号。
    pub line: usize,
    /// 匹配行内容。
    pub text: String,
    /// 前置上下文（仅当 backend 被要求 context_lines>0）。
    pub context_before: Option<Vec<ContextLine>>,
    /// 后置上下文。
    pub context_after: Option<Vec<ContextLine>>,
}

/// 文件内容 + 元数据。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileData {
    /// 文本（utf-8）或 base64 编码二进制。
    pub content: String,
    /// `"utf-8"` 或 `"base64"`。
    pub encoding: String,
    /// ISO 8601 创建时间。
    pub created_at: Option<String>,
    /// ISO 8601 修改时间。
    pub modified_at: Option<String>,
}

/// `read` 结果。
#[derive(Clone, Debug, Default)]
pub struct ReadResult {
    /// 失败信息，None=成功。
    pub error: Option<String>,
    /// 成功时的文件数据。
    pub file_data: Option<FileData>,
    /// 源文件总行数（若 backend 可确定）。
    pub total_lines: Option<usize>,
    /// 1-indexed 首行。
    pub start_line: Option<usize>,
    /// 1-indexed 末行。
    pub end_line: Option<usize>,
    /// 0-indexed 续读偏移（== end_line）。
    pub next_offset: Option<usize>,
    /// 非正 limit 短路（文件未检视）。
    pub no_lines_requested: bool,
}

impl ReadResult {
    /// 成功构造（含分页元数据）。
    #[must_use]
    pub fn ok(file_data: FileData, total_lines: usize, start_line: usize, end_line: usize, next_offset: Option<usize>) -> Self {
        Self { error: None, file_data: Some(file_data), total_lines: Some(total_lines), start_line: Some(start_line), end_line: Some(end_line), next_offset, no_lines_requested: false }
    }
    /// 失败构造。
    #[must_use]
    pub fn err(msg: impl Into<String>) -> Self {
        Self { error: Some(msg.into()), ..Default::default() }
    }
}

/// `write` 结果。
#[derive(Clone, Debug, Default)]
pub struct WriteResult { pub error: Option<String>, pub path: Option<String> }
impl WriteResult {
    #[must_use] pub fn ok(path: impl Into<String>) -> Self { Self { error: None, path: Some(path.into()) } }
    #[must_use] pub fn err(msg: impl Into<String>) -> Self { Self { error: Some(msg.into()), path: None } }
}

/// `edit` 结果。
#[derive(Clone, Debug, Default)]
pub struct EditResult { pub error: Option<String>, pub path: Option<String>, pub occurrences: Option<usize> }
impl EditResult {
    #[must_use] pub fn ok(path: impl Into<String>, occurrences: usize) -> Self { Self { error: None, path: Some(path.into()), occurrences: Some(occurrences) } }
    #[must_use] pub fn err(msg: impl Into<String>) -> Self { Self { error: Some(msg.into()), path: None, occurrences: None } }
}

/// `delete` 结果。
#[derive(Clone, Debug, Default)]
pub struct DeleteResult { pub error: Option<String>, pub path: Option<String> }
impl DeleteResult {
    #[must_use] pub fn ok(path: impl Into<String>) -> Self { Self { error: None, path: Some(path.into()) } }
    #[must_use] pub fn err(msg: impl Into<String>) -> Self { Self { error: Some(msg.into()), path: None } }
}

/// `ls` 结果。
#[derive(Clone, Debug, Default)]
pub struct LsResult { pub error: Option<String>, pub entries: Option<Vec<FileInfo>> }
impl LsResult {
    #[must_use] pub fn ok(entries: Vec<FileInfo>) -> Self { Self { error: None, entries: Some(entries) } }
    #[must_use] pub fn err(msg: impl Into<String>) -> Self { Self { error: Some(msg.into()), entries: None } }
}

/// `grep` 结果。
#[derive(Clone, Debug, Default)]
pub struct GrepResult { pub error: Option<String>, pub matches: Option<Vec<GrepMatch>>, pub truncated: bool }
impl GrepResult {
    #[must_use] pub fn ok(matches: Vec<GrepMatch>) -> Self { Self { error: None, matches: Some(matches), truncated: false } }
    #[must_use] pub fn err(msg: impl Into<String>) -> Self { Self { error: Some(msg.into()), matches: None, truncated: false } }
    /// 截断（达到 max_count）。
    #[must_use]
    pub fn truncated(matches: Vec<GrepMatch>) -> Self { Self { error: None, matches: Some(matches), truncated: true } }
}

/// `glob` 结果。
#[derive(Clone, Debug, Default)]
pub struct GlobResult { pub error: Option<String>, pub matches: Option<Vec<FileInfo>>, pub truncated: bool }
impl GlobResult {
    #[must_use] pub fn ok(matches: Vec<FileInfo>) -> Self { Self { error: None, matches: Some(matches), truncated: false } }
    #[must_use] pub fn err(msg: impl Into<String>) -> Self { Self { error: Some(msg.into()), matches: None, truncated: false } }
}

/// `execute` 结果（合并 stdout+stderr）。
#[derive(Clone, Debug, Default)]
pub struct ExecuteResponse { pub output: String, pub exit_code: Option<i32>, pub truncated: bool }
impl ExecuteResponse {
    #[must_use] pub fn new(output: impl Into<String>, exit_code: Option<i32>) -> Self { Self { output: output.into(), exit_code, truncated: false } }
}

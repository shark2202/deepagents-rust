//! 8 个文件系统工具 —— 对应 deepagents `FsToolName`（ls/read_file/write_file/edit_file/
//! glob/grep/delete/execute）。
//!
//! 每个工具持 `Arc<dyn Backend>`，作为 juncture `Tool`。`FilesystemMiddleware::tools()` 返回
//! 这 8 个；`wrap_model_call` 按 `backend.supported_tools()` per-call 过滤可见性（capability gating）。
//! 工具失败返回 `Ok("Error: ...")`（对齐 deepagents `ToolMessage(status="error")`，
//! 也兼容 juncture `ToolErrorHandlingMiddleware` 的 "Error:" 前缀识别）。

use std::sync::Arc;

use async_trait::async_trait;
use juncture::tools::{Tool, ToolError};
use serde_json::{json, Value};

use crate::backend::{Backend, FileInfo};

// ===== helper =====

/// 格式化路径列表（ls/glob 用）。
fn format_paths(paths: Vec<String>) -> String {
    if paths.is_empty() {
        "No files found".to_string()
    } else {
        paths.join("\n")
    }
}

/// 从 FileInfo 提取 path。
fn paths_from_infos(infos: Vec<FileInfo>) -> Vec<String> {
    infos.into_iter().map(|f| f.path).collect()
}

/// 解析字符串字段。
fn get_str(input: &Value, field: &str) -> Result<String, ToolError> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::invalid_input(format!("missing or invalid '{field}'")))
}

fn get_opt_str(input: &Value, field: &str) -> Option<String> {
    input.get(field).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn get_opt_u64(input: &Value, field: &str) -> Option<u64> {
    input.get(field).and_then(|v| v.as_u64())
}

fn get_opt_bool(input: &Value, field: &str) -> Option<bool> {
    input.get(field).and_then(|v| v.as_bool())
}

/// 把 backend error 转成 "Error: ..." 字符串。
fn err_str(e: impl std::fmt::Display) -> String {
    format!("Error: {e}")
}

// ===== ls =====

/// 列目录。
pub struct LsTool {
    backend: Arc<dyn Backend>,
}

impl LsTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &'static str {
        "ls"
    }
    fn description(&self) -> &'static str {
        "List all files in a directory with metadata. Returns file paths."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"path":{"type":"string","description":"Absolute path to the directory to list."}},"required":["path"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let path = get_str(&input, "path")?;
        let r = self.backend.ls(&path).await;
        match r.error {
            Some(e) => Ok(err_str(e)),
            None => Ok(format_paths(paths_from_infos(r.entries.unwrap_or_default()))),
        }
    }
}

// ===== read_file =====

/// 读文件内容（带行号 + 分页）。
pub struct ReadFileTool {
    backend: Arc<dyn Backend>,
}

impl ReadFileTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read file content with line numbers. Supports offset (0-indexed start line) and limit (max lines) for pagination."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"file_path":{"type":"string"},"offset":{"type":"integer","default":0},"limit":{"type":"integer","default":100}},"required":["file_path"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let file_path = get_str(&input, "file_path")?;
        let offset = get_opt_u64(&input, "offset").unwrap_or(0) as usize;
        let limit = get_opt_u64(&input, "limit").unwrap_or(100) as usize;
        let r = self.backend.read(&file_path, offset, limit).await;
        if let Some(e) = r.error {
            return Ok(err_str(e));
        }
        let fd = match r.file_data {
            Some(d) => d,
            None => return Ok(format!("Error: no data returned for '{file_path}'")),
        };
        if fd.content.is_empty() {
            return Ok("System reminder: File exists but has empty contents".to_string());
        }
        let start = r.start_line.unwrap_or(offset + 1);
        let formatted = fd
            .content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{}\t{}", start + i, line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(formatted)
    }
}

// ===== write_file =====

/// 写文件（创建或覆盖）。
pub struct WriteFileTool {
    backend: Arc<dyn Backend>,
}

impl WriteFileTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Write content to a file, creating it or overwriting if it exists."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"file_path":{"type":"string"},"content":{"type":"string"}},"required":["file_path","content"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let file_path = get_str(&input, "file_path")?;
        let content = get_str(&input, "content")?;
        let r = self.backend.write(&file_path, &content).await;
        match (r.error, r.path) {
            (Some(e), _) => Ok(err_str(e)),
            (None, Some(p)) => Ok(format!("Updated file {p}")),
            (None, None) => Ok("Error: write returned no path".to_string()),
        }
    }
}

// ===== edit_file =====

/// 精确字符串替换。
pub struct EditFileTool {
    backend: Arc<dyn Backend>,
}

impl EditFileTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn description(&self) -> &'static str {
        "Perform exact string replacement in a file. old_string must be unique unless replace_all=true."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"file_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"replace_all":{"type":"boolean","default":false}},"required":["file_path","old_string","new_string"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let file_path = get_str(&input, "file_path")?;
        let old_string = get_str(&input, "old_string")?;
        let new_string = get_str(&input, "new_string")?;
        let replace_all = get_opt_bool(&input, "replace_all").unwrap_or(false);
        let r = self
            .backend
            .edit(&file_path, &old_string, &new_string, replace_all)
            .await;
        match (r.error, r.path, r.occurrences) {
            (Some(e), _, _) => Ok(err_str(e)),
            (None, Some(p), Some(n)) => Ok(format!("Successfully replaced {n} instance(s) of the string in '{p}'")),
            _ => Ok("Error: edit returned no result".to_string()),
        }
    }
}

// ===== glob =====

/// glob 匹配文件。
pub struct GlobTool {
    backend: Arc<dyn Backend>,
}

impl GlobTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "Find files matching a glob pattern (supports * and **)."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let pattern = get_str(&input, "pattern")?;
        let path = get_opt_str(&input, "path");
        let r = self.backend.glob(&pattern, path.as_deref()).await;
        if let Some(e) = r.error {
            return Ok(err_str(e));
        }
        let mut paths = paths_from_infos(r.matches.unwrap_or_default());
        let mut out = format_paths(std::mem::take(&mut paths));
        if r.truncated {
            out.push_str("\n\n[Results truncated due to size limits]");
        }
        Ok(out)
    }
}

// ===== grep =====

/// 字面量文本搜索（非 regex）。
pub struct GrepTool {
    backend: Arc<dyn Backend>,
}

impl GrepTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search for a literal text pattern in files (not regex). output_mode: files_with_matches | content | count."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"},"output_mode":{"type":"string","enum":["files_with_matches","content","count"],"default":"files_with_matches"},"max_count":{"type":"integer"}},"required":["pattern"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let pattern = get_str(&input, "pattern")?;
        let path = get_opt_str(&input, "path");
        let glob = get_opt_str(&input, "glob");
        let output_mode = get_opt_str(&input, "output_mode").unwrap_or_else(|| "files_with_matches".to_string());
        let max_count = get_opt_u64(&input, "max_count").map(|n| n as usize);
        let r = self
            .backend
            .grep(&pattern, path.as_deref(), glob.as_deref(), max_count)
            .await;
        if let Some(e) = r.error {
            return Ok(err_str(e));
        }
        let matches = r.matches.unwrap_or_default();
        let out = match output_mode.as_str() {
            "count" => {
                // 按文件聚合 count
                use std::collections::BTreeMap;
                let mut counts: BTreeMap<String, usize> = BTreeMap::new();
                for m in &matches {
                    *counts.entry(m.path.clone()).or_default() += 1;
                }
                counts.into_iter().map(|(p, n)| format!("{p}:{n}")).collect::<Vec<_>>().join("\n")
            }
            "content" => matches
                .iter()
                .map(|m| format!("{}:{}:{}", m.path, m.line, m.text))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => {
                // files_with_matches：unique paths
                let mut seen = Vec::new();
                for m in &matches {
                    if !seen.contains(&m.path) {
                        seen.push(m.path.clone());
                    }
                }
                if seen.is_empty() {
                    "No files found".to_string()
                } else {
                    seen.join("\n")
                }
            }
        };
        let mut out = out;
        if r.truncated {
            out.push_str("\n\n[Results truncated due to size limits]");
        }
        Ok(out)
    }
}

// ===== delete =====

/// 递归删除。
pub struct DeleteTool {
    backend: Arc<dyn Backend>,
}

impl DeleteTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for DeleteTool {
    fn name(&self) -> &'static str {
        "delete"
    }
    fn description(&self) -> &'static str {
        "Delete a file or directory (recursive)."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"file_path":{"type":"string"}},"required":["file_path"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let file_path = get_str(&input, "file_path")?;
        let r = self.backend.delete(&file_path).await;
        match (r.error, r.path) {
            (Some(e), _) => Ok(err_str(e)),
            (None, Some(p)) => Ok(format!("Deleted {p}")),
            (None, None) => Ok("Error: delete returned no path".to_string()),
        }
    }
}

// ===== execute =====

/// 执行 shell 命令（需 SandboxBackend）。
pub struct ExecuteTool {
    backend: Arc<dyn Backend>,
}

impl ExecuteTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for ExecuteTool {
    fn name(&self) -> &'static str {
        "execute"
    }
    fn description(&self) -> &'static str {
        "Execute a shell command in the sandbox. timeout in seconds (0=unlimited on supporting backends)."
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer"}},"required":["command"]})
    }
    async fn invoke(&self, input: Value) -> Result<String, ToolError> {
        let command = get_str(&input, "command")?;
        let timeout = get_opt_u64(&input, "timeout").map(|n| n as u32);
        // timeout 校验（对齐 deepagents：0..=3600）
        if let Some(t) = timeout
            && t > 3600
        {
            return Ok(format!("Error: timeout {t}s exceeds maximum allowed (3600s)."));
        }
        let sandbox = match self.backend.as_sandbox() {
            Some(s) => s,
            None => return Ok("Error: Execution not available: backend does not support command execution (SandboxBackend).".to_string()),
        };
        let r = sandbox.execute(&command, timeout).await;
        let mut out = r.output;
        if let Some(code) = r.exit_code {
            let status = if code == 0 { "succeeded" } else { "failed" };
            out.push_str(&format!("\n[Command {status} with exit code {code}]"));
        }
        if r.truncated {
            out.push_str("\n[Output was truncated due to size limits]");
        }
        Ok(out)
    }
}

/// 构造全部 8 个 fs 工具（持 backend clone）。构造顺序对齐 deepagents：ls, read_file, write_file, edit_file, delete, glob, grep, execute。
#[must_use]
pub fn all_fs_tools(backend: Arc<dyn Backend>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(LsTool::new(Arc::clone(&backend))),
        Box::new(ReadFileTool::new(Arc::clone(&backend))),
        Box::new(WriteFileTool::new(Arc::clone(&backend))),
        Box::new(EditFileTool::new(Arc::clone(&backend))),
        Box::new(DeleteTool::new(Arc::clone(&backend))),
        Box::new(GlobTool::new(Arc::clone(&backend))),
        Box::new(GrepTool::new(Arc::clone(&backend))),
        Box::new(ExecuteTool::new(backend)),
    ]
}

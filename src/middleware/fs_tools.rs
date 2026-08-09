//! 8 个文件系统工具 —— 对应 deepagents `FsToolName`（ls/read_file/write_file/edit_file/
//! glob/grep/delete/execute）。
//!
//! 每个工具持 `Arc<dyn Backend>` + `Arc<[FilesystemPermission]>`。invoke 顺序：
//! parse args → `check_interruptible`（`check_fs_permission` 返回 `PermissionMode`：
//! `Allow` → 继续；`Deny` → "Error: permission denied..."；`Interrupt` → 调
//! `juncture::interrupt_with_ctx!` 触发 HITL 暂停，match resume decision）
//! → backend call → 格式化。
//!
//! # HITL（deepagents 第 8 大特性）
//!
//! `Interrupt` 模式真正生效：工具 invoke 内取 Pregel task-local `InterruptContext`（runner
//! 在 node 执行前 `INTERRUPT_CONTEXT.scope`），调 `juncture::interrupt_with_ctx!`，payload 含
//! tool/operation/path 供 human 决策。`ToolNode` 在 Pregel node 内执行，故工具 invoke 内 task-local
//! 已设。首次执行发 `InterruptSignal` 到 channel 并返回 `Err(JunctureError::interrupted)`；
//! resume 后 Pregel 重跑 node，`interrupt_with_ctx!` 返回 `Ok(resume_value)`。工具 catch
//! 任何 `Err`（含 task-local 未设的单元测试场景）返回 `Ok("Error: ...")` 而非
//! panic——Pregel 通过 after-superstep channel drain 检测 interrupt signal 并暂停，
//! 与工具返回值无关。
//!
//! 用 `interrupt_with_ctx!` 而非 task-local `interrupt!`：后者在 `try_with` 闭包内含
//! `.await`（非 async 闭包不可编译），且 juncture 自身测试也用 `interrupt_with_ctx!`
//! （见 juncture-core `interrupt_tests.rs`）。
//!
//! 工具失败返回 `Ok("Error: ...")`（对齐 deepagents `ToolMessage(status="error")`，
//! 也兼容 juncture `ToolErrorHandlingMiddleware` 的 "Error:" 前缀识别）。
//! post-filter（ls/glob/grep drop deny entries）defer 到增量 C2。

use std::sync::Arc;

use async_trait::async_trait;
use juncture::tools::{Tool, ToolError};
use serde_json::{Value, json};

use crate::backend::{Backend, FileInfo};
use crate::permission::{
    FilesystemOperation, FilesystemPermission, PermissionMode, check_fs_permission,
};

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
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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

/// 解析 human 的 resume decision（来自 `interrupt!` 返回的 `serde_json::Value`）。
///
/// 接受两种形式：
/// - 字符串：`"approve"` / `"reject"` / `"edit"` / `"respond"`
/// - 对象带 `"decision"` 字符串字段：`{"decision": "approve", ...}`
///
/// 决策映射（对齐 deepagents HITL，简化处理 edit/respond）：
/// - `"approve"` / null（默认 resume，human 未指定） → `Approve`（继续工具执行）
/// - `"edit"` → `Approve`（简化：当作 approve，用原 args；defer 真实 edit）
/// - `"reject"` → `Reject`（拒绝）
/// - `"respond"` → `Reject`（简化：当作 reject；defer 真实 respond）
/// - 其他 → `Invalid`
fn parse_resume_decision(resume: &Value) -> ResumeDecision {
    let decision = match resume {
        // human 直接 resume 未指定值（Null）→ 默认 approve（继续）。
        Value::Null => return ResumeDecision::Approve,
        Value::String(s) => s.as_str(),
        Value::Object(map) => match map.get("decision").and_then(Value::as_str) {
            Some(s) => s,
            None => return ResumeDecision::Invalid,
        },
        _ => return ResumeDecision::Invalid,
    };
    match decision {
        "approve" | "edit" => ResumeDecision::Approve,
        "reject" | "respond" => ResumeDecision::Reject,
        _ => ResumeDecision::Invalid,
    }
}

/// Resume decision outcome。
enum ResumeDecision {
    /// 继续 backend call（approve / edit-simplified / null-default）。
    Approve,
    /// 拒绝（reject / respond-simplified）。
    Reject,
    /// 无法识别的 decision。
    Invalid,
}

/// 权限 + HITL 检查：`Ok(None)` 继续 backend；`Ok(Some(s))` 短路返回 Ok(s) 给模型；
/// `Err(ToolError)` propagate —— 让 ToolNode 失败、不产出 writes，Pregel 检测 interrupt
/// signal 后 pause，resume 时重跑本节点，`interrupt_with_ctx!` 返回 Ok(resume_value)。
///
/// - `Allow` → `Ok(None)`
/// - `Deny` → `Ok(Some("Error: permission denied..."))`
/// - `Interrupt` → 取 task-local `InterruptContext`，调 `interrupt_with_ctx!`：
///   - `Ok(resume)` → match decision: Approve→`Ok(None)`；Reject/Invalid→`Ok(Some(...))`
///   - `Err(juncture_err)` → `Err(ToolError)` propagate（首次 interrupt 已发 signal，Pregel
///     after_tick drain channel 检测 → InterruptAfter pause）
///   - task-local 未设 → `Err(ToolError)` propagate（非 Pregel 上下文无 resume，工具失败）
async fn check_interruptible(
    perms: &[FilesystemPermission],
    op: FilesystemOperation,
    path: &str,
    tool_name: &str,
) -> Result<Option<String>, ToolError> {
    match check_fs_permission(perms, op, path) {
        PermissionMode::Allow => Ok(None),
        PermissionMode::Deny => Ok(Some(format!("Error: permission denied for {op} on {path}"))),
        PermissionMode::Interrupt => {
            let payload = json!({
                "tool": tool_name,
                "operation": op,
                "path": path,
            });
            match juncture::interrupt::INTERRUPT_CONTEXT.try_with(Arc::clone) {
                Ok(ctx) => match juncture::interrupt_with_ctx!(&ctx, payload) {
                    Ok(resume) => match parse_resume_decision(&resume) {
                        ResumeDecision::Approve => Ok(None),
                        ResumeDecision::Reject => {
                            Ok(Some(format!("Error: {tool_name} rejected by human")))
                        }
                        ResumeDecision::Invalid => {
                            Ok(Some("Error: invalid resume decision".to_string()))
                        }
                    },
                    // propagate interrupt Err —— ToolNode 失败，不 merge writes，
                    // Pregel after_tick drain channel 检测 signal → pause + resume 重跑。
                    Err(e) => Err(ToolError::ExecutionFailed(format!("HITL interrupt: {e}"))),
                },
                Err(_) => Err(ToolError::ExecutionFailed(
                    "HITL interrupt: context not set in task-local".to_string(),
                )),
            }
        }
    }
}

// ===== ls =====

/// 列目录。
pub struct LsTool {
    backend: Arc<dyn Backend>,
    permissions: Arc<[FilesystemPermission]>,
}

impl LsTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        match check_interruptible(
            &self.permissions,
            FilesystemOperation::Read,
            &path,
            self.name(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(e)) => return Ok(e),
            Err(e) => return Err(e),
        }
        let r = self.backend.ls(&path).await;
        match r.error {
            Some(e) => Ok(err_str(e)),
            None => Ok(format_paths(paths_from_infos(
                r.entries.unwrap_or_default(),
            ))),
        }
    }
}

// ===== read_file =====

/// 读文件内容（带行号 + 分页）。
pub struct ReadFileTool {
    backend: Arc<dyn Backend>,
    permissions: Arc<[FilesystemPermission]>,
}

impl ReadFileTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        match check_interruptible(
            &self.permissions,
            FilesystemOperation::Read,
            &file_path,
            self.name(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(e)) => return Ok(e),
            Err(e) => return Err(e),
        }
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
    permissions: Arc<[FilesystemPermission]>,
}

impl WriteFileTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        match check_interruptible(
            &self.permissions,
            FilesystemOperation::Write,
            &file_path,
            self.name(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(e)) => return Ok(e),
            Err(e) => return Err(e),
        }
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
    permissions: Arc<[FilesystemPermission]>,
}

impl EditFileTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        match check_interruptible(
            &self.permissions,
            FilesystemOperation::Write,
            &file_path,
            self.name(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(e)) => return Ok(e),
            Err(e) => return Err(e),
        }
        let old_string = get_str(&input, "old_string")?;
        let new_string = get_str(&input, "new_string")?;
        let replace_all = get_opt_bool(&input, "replace_all").unwrap_or(false);
        let r = self
            .backend
            .edit(&file_path, &old_string, &new_string, replace_all)
            .await;
        match (r.error, r.path, r.occurrences) {
            (Some(e), _, _) => Ok(err_str(e)),
            (None, Some(p), Some(n)) => Ok(format!(
                "Successfully replaced {n} instance(s) of the string in '{p}'"
            )),
            _ => Ok("Error: edit returned no result".to_string()),
        }
    }
}

// ===== glob =====

/// glob 匹配文件。
pub struct GlobTool {
    backend: Arc<dyn Backend>,
    permissions: Arc<[FilesystemPermission]>,
}

impl GlobTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        // permission check on base path（对齐 deepagents: validate_path(path or "/")）
        let perm_path = path.as_deref().unwrap_or("/");
        match check_interruptible(
            &self.permissions,
            FilesystemOperation::Read,
            perm_path,
            self.name(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(e)) => return Ok(e),
            Err(e) => return Err(e),
        }
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
    permissions: Arc<[FilesystemPermission]>,
}

impl GrepTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        let output_mode =
            get_opt_str(&input, "output_mode").unwrap_or_else(|| "files_with_matches".to_string());
        let max_count = get_opt_u64(&input, "max_count").map(|n| n as usize);
        // permission check：仅当 path is Some（对齐 deepagents：path=None 时不 pre-check，走 post-filter）
        if let Some(p) = &path {
            match check_interruptible(&self.permissions, FilesystemOperation::Read, p, self.name())
                .await
            {
                Ok(None) => {}
                Ok(Some(e)) => return Ok(e),
                Err(e) => return Err(e),
            }
        }
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
                use std::collections::BTreeMap;
                let mut counts: BTreeMap<String, usize> = BTreeMap::new();
                for m in &matches {
                    *counts.entry(m.path.clone()).or_default() += 1;
                }
                counts
                    .into_iter()
                    .map(|(p, n)| format!("{p}:{n}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "content" => matches
                .iter()
                .map(|m| format!("{}:{}:{}", m.path, m.line, m.text))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => {
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
    permissions: Arc<[FilesystemPermission]>,
}

impl DeleteTool {
    #[must_use]
    pub fn new(backend: Arc<dyn Backend>, permissions: Arc<[FilesystemPermission]>) -> Self {
        Self {
            backend,
            permissions,
        }
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
        // deepagents 用 conservative subtree check（_find_delete_deny_patterns）；MVP 简化为 check_fs_permission on file_path。
        match check_interruptible(
            &self.permissions,
            FilesystemOperation::Write,
            &file_path,
            self.name(),
        )
        .await
        {
            Ok(None) => {}
            Ok(Some(e)) => return Ok(e),
            Err(e) => return Err(e),
        }
        let r = self.backend.delete(&file_path).await;
        match (r.error, r.path) {
            (Some(e), _) => Ok(err_str(e)),
            (None, Some(p)) => Ok(format!("Deleted {p}")),
            (None, None) => Ok("Error: delete returned no path".to_string()),
        }
    }
}

// ===== execute =====

/// 执行 shell 命令（需 SandboxBackend）。execute 无 permission 检查（对齐 deepagents spec）。
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
            return Ok(format!(
                "Error: timeout {t}s exceeds maximum allowed (3600s)."
            ));
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

/// 构造全部 8 个 fs 工具（持 backend + permissions clone）。
/// 构造顺序对齐 deepagents：ls, read_file, write_file, edit_file, delete, glob, grep, execute。
#[must_use]
pub fn all_fs_tools(
    backend: Arc<dyn Backend>,
    permissions: Arc<[FilesystemPermission]>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(LsTool::new(Arc::clone(&backend), Arc::clone(&permissions))),
        Box::new(ReadFileTool::new(
            Arc::clone(&backend),
            Arc::clone(&permissions),
        )),
        Box::new(WriteFileTool::new(
            Arc::clone(&backend),
            Arc::clone(&permissions),
        )),
        Box::new(EditFileTool::new(
            Arc::clone(&backend),
            Arc::clone(&permissions),
        )),
        Box::new(DeleteTool::new(
            Arc::clone(&backend),
            Arc::clone(&permissions),
        )),
        Box::new(GlobTool::new(
            Arc::clone(&backend),
            Arc::clone(&permissions),
        )),
        Box::new(GrepTool::new(
            Arc::clone(&backend),
            Arc::clone(&permissions),
        )),
        Box::new(ExecuteTool::new(backend)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::WriteResult;

    /// 记录 write 调用的 mock backend（验证 Allow 继续执行 / Deny 短路不触达 backend）。
    #[derive(Debug, Default)]
    struct MockBackend {
        wrote: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Backend for MockBackend {
        async fn write(&self, file_path: &str, content: &str) -> WriteResult {
            self.wrote
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((file_path.to_string(), content.to_string()));
            WriteResult::ok(file_path.to_string())
        }
    }

    fn perm_interrupt() -> Arc<[FilesystemPermission]> {
        vec![
            FilesystemPermission::interrupt(
                vec![FilesystemOperation::Write],
                vec!["/review/**".into()],
            )
            .unwrap(),
        ]
        .into()
    }

    fn perm_deny() -> Arc<[FilesystemPermission]> {
        vec![
            FilesystemPermission::deny(vec![FilesystemOperation::Write], vec!["/secret/**".into()])
                .unwrap(),
        ]
        .into()
    }

    fn perm_allow_all() -> Arc<[FilesystemPermission]> {
        vec![
            FilesystemPermission::allow(
                vec![FilesystemOperation::Write, FilesystemOperation::Read],
                vec!["/**".into()],
            )
            .unwrap(),
        ]
        .into()
    }

    // --- Deny: 返回 error，不调 interrupt（无 Pregel task-local 也能跑） ---

    #[tokio::test]
    async fn deny_returns_error_without_interrupt() {
        let backend = Arc::new(MockBackend::default());
        let tool = WriteFileTool::new(Arc::clone(&backend) as Arc<dyn Backend>, perm_deny());
        let r = tool
            .invoke(json!({"file_path": "/secret/x", "content": "hi"}))
            .await
            .expect("invoke returns Ok");
        assert!(
            r.starts_with("Error: permission denied for write on /secret/x"),
            "deny 返回 permission denied error，got: {r}"
        );
        // backend 未被触达（短路在 permission check）。
        assert!(
            backend
                .wrote
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "deny 短路不应触达 backend"
        );
    }

    // --- Allow: 继续 backend call ---

    #[tokio::test]
    async fn allow_continues_to_backend() {
        let backend = Arc::new(MockBackend::default());
        let tool = WriteFileTool::new(Arc::clone(&backend) as Arc<dyn Backend>, perm_allow_all());
        let r = tool
            .invoke(json!({"file_path": "/public/x", "content": "hi"}))
            .await
            .expect("invoke returns Ok");
        assert_eq!(r, "Updated file /public/x");
        let wrote = backend
            .wrote
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(wrote, vec![("/public/x".to_string(), "hi".to_string())]);
    }

    // --- Interrupt 无 Pregel task-local：catch Err 返回 Ok("Error:...")，不 panic ---

    #[tokio::test]
    async fn interrupt_without_pregel_context_propagates_err_not_panic() {
        // 单元测试直接调 invoke，未在 Pregel node 内 → INTERRUPT_CONTEXT task-local 未设 →
        // try_with 返回 Err(AccessError) → 工具 propagate `Err(ToolError)`（让 Pregel pause；
        // 非 Pregel 上下文工具失败合理——无 resume 机制）。不 panic。
        let backend = Arc::new(MockBackend::default());
        let tool = WriteFileTool::new(Arc::clone(&backend) as Arc<dyn Backend>, perm_interrupt());
        let r = tool
            .invoke(json!({"file_path": "/review/x", "content": "hi"}))
            .await;
        assert!(r.is_err(), "propagate Err 非 catch，got: {r:?}");
        let err = r.unwrap_err();
        assert!(
            format!("{err}").contains("context not set"),
            "应含 context not set，got: {err}"
        );
        // backend 未触达。
        assert!(
            backend
                .wrote
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "interrupt 未 resume 时不应触达 backend"
        );
    }

    // --- 正交规则：Deny 不命中路径 → Allow 继续 ---

    #[tokio::test]
    async fn deny_rule_only_matches_its_paths() {
        let backend = Arc::new(MockBackend::default());
        let tool = WriteFileTool::new(Arc::clone(&backend) as Arc<dyn Backend>, perm_deny());
        // /secret/** deny 不命中 /public/x。
        let r = tool
            .invoke(json!({"file_path": "/public/x", "content": "hi"}))
            .await
            .expect("invoke returns Ok");
        // 继续到 backend（default Allow）。
        assert_eq!(r, "Updated file /public/x");
    }

    // --- parse_resume_decision 纯逻辑测试 ---

    #[test]
    fn parse_resume_decision_approve_and_null() {
        assert!(matches!(
            parse_resume_decision(&json!("approve")),
            ResumeDecision::Approve
        ));
        // Null（human 直接 resume 未指定值）→ 默认 approve。
        assert!(matches!(
            parse_resume_decision(&Value::Null),
            ResumeDecision::Approve
        ));
        // edit 简化为 approve。
        assert!(matches!(
            parse_resume_decision(&json!("edit")),
            ResumeDecision::Approve
        ));
    }

    #[test]
    fn parse_resume_decision_reject_and_respond() {
        assert!(matches!(
            parse_resume_decision(&json!("reject")),
            ResumeDecision::Reject
        ));
        // respond 简化为 reject。
        assert!(matches!(
            parse_resume_decision(&json!("respond")),
            ResumeDecision::Reject
        ));
    }

    #[test]
    fn parse_resume_decision_object_form() {
        assert!(matches!(
            parse_resume_decision(&json!({"decision": "approve"})),
            ResumeDecision::Approve
        ));
        assert!(matches!(
            parse_resume_decision(&json!({"decision": "reject", "reason": "no"})),
            ResumeDecision::Reject
        ));
        // object 无 decision 字段 → Invalid。
        assert!(matches!(
            parse_resume_decision(&json!({"foo": "bar"})),
            ResumeDecision::Invalid
        ));
        // decision 非 string → Invalid。
        assert!(matches!(
            parse_resume_decision(&json!({"decision": 42})),
            ResumeDecision::Invalid
        ));
    }

    #[test]
    fn parse_resume_decision_invalid() {
        // 未识别的字符串。
        assert!(matches!(
            parse_resume_decision(&json!("maybe")),
            ResumeDecision::Invalid
        ));
        // 数字 → Invalid。
        assert!(matches!(
            parse_resume_decision(&json!(42)),
            ResumeDecision::Invalid
        ));
        // 数组 → Invalid。
        assert!(matches!(
            parse_resume_decision(&json!(["approve"])),
            ResumeDecision::Invalid
        ));
    }

    // --- 端到端 HITL（Pregel interrupt + resume）测试 defer ---
    //
    // 真实端到端 HITL 测试需：
    //   1. 构造 DeepAgent（含 Interrupt 权限的 FilesystemMiddleware）
    //   2. 配 checkpointer（resume 要求 Interrupt-source checkpoint）
    //   3. invoke → tools 节点内 interrupt_with_ctx! 发信号 → Pregel after-superstep
    //      drain channel 暂停（LoopStatus::InterruptAfter）
    //   4. resume(ResumeValue::Single(json!("approve"|"reject"))) → Pregel 重跑
    //      tools node → interrupt_with_ctx! 返 Ok(resume_value) → match decision
    //   5. assert 暂停时 GraphOutput.interrupts 非空；resume 后 write_file 触达/reject 短路
    //
    // 此链路涉及 juncture Pregel runtime + checkpointer + ToolNode + interrupt context
    // 协同，复杂度高，defer 到集成测试套件（需 MockChatModel 驱动 tool_call + 内存
    // checkpointer）。当前单元测试覆盖：Deny 短路、Allow 继续、Interrupt 无 ctx 不 panic、
    // decision 解析逻辑。
}

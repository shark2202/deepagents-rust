//! Middleware components: Filesystem, SubAgent, Summarization (Q5).
//!
//! Each middleware is a component struct that contributes (a) a set of tools
//! and (b) an `impl AgentHook` for lifecycle interception. The HITL
//! middleware is in [`crate::hitl`].
//!
//! See `docs/SPEC.md` §Q5 for the mapping table and design rationale.

use std::sync::Arc;

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, CompletionResponseEvent, HookContext,
    InvalidToolCallAction, ModelSelection, ModelSelectionAction, ModelTurnAction, ModelTurnFinished,
    ObservationAction, RequestPatch, StreamResponseFinish, TextDelta, ReasoningDelta,
    ToolCallAction, ToolCall as RigToolCall, ToolCallDelta, ToolResultAction, ToolResultEvent,
};
use rig_core::completion::Message;
use rig_core::wasm_compat::WasmCompatSend;

use crate::backend::Backend;
use crate::permission::{FilesystemOperation, FilesystemPermission, PermissionChecker, PermissionMode};

// ── FilesystemMiddleware ──────────────────────────────────────────────

/// The filesystem middleware: contributes file tools and intercepts
/// completion calls to inject filesystem instructions and dynamically
/// filter tools. On `on_tool_result`, large results are evicted to a file.
///
/// Mapping (SPEC Q5):
/// - `on_completion_call`: inject fs instructions + dynamic tool filtering
/// - `on_tool_result`: evict large results to file
pub struct FilesystemMiddleware {
    /// The backend providing file operations.
    backend: Arc<dyn Backend>,
    /// Filesystem permission rules (for tool execution, not hook logic).
    permissions: Vec<FilesystemPermission>,
    /// Pre-built permission checker for efficient rule evaluation.
    permission_checker: PermissionChecker,
    /// Whether the backend supports shell execution.
    has_sandbox: bool,
    /// Threshold (in bytes) above which a tool result is evicted to a file.
    /// Default: 10_000 bytes.
    eviction_threshold: usize,
}

impl FilesystemMiddleware {
    /// Create a new filesystem middleware.
    pub fn new(backend: Arc<dyn Backend>, permissions: Vec<FilesystemPermission>) -> Self {
        let permission_checker = PermissionChecker::new(permissions.clone());
        Self {
            backend,
            permissions,
            permission_checker,
            has_sandbox: false,
            eviction_threshold: 10_000,
        }
    }

    /// Create a filesystem middleware with no permissions (allow all).
    pub fn permissive(backend: Arc<dyn Backend>) -> Self {
        Self::new(backend, vec![])
    }

    /// Explicitly set whether the backend supports shell execution.
    pub fn with_sandbox(mut self, has_sandbox: bool) -> Self {
        self.has_sandbox = has_sandbox;
        self
    }

    /// Set the eviction threshold (bytes).
    pub fn with_eviction_threshold(mut self, threshold: usize) -> Self {
        self.eviction_threshold = threshold;
        self
    }

    /// Returns `true` if the backend supports shell execution.
    pub fn has_sandbox(&self) -> bool {
        self.has_sandbox
    }

    /// Get the backend reference.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    /// Get the permission rules.
    pub fn permissions(&self) -> &[FilesystemPermission] {
        &self.permissions
    }

    /// The filesystem system prompt injected into completion calls.
    pub fn fs_system_prompt(&self) -> &'static str {
        "You have access to a virtual filesystem. All paths are POSIX-style \
         (forward slashes, absolute starting with /). You can read, write, \
         edit, list, search (grep), and glob files. Use these tools to \
         explore and modify the codebase."
    }

    /// The list of tool names this middleware contributes.
    pub fn tool_names(&self) -> Vec<&'static str> {
        let mut tools = vec![
            "write_file",
            "read_file",
            "edit_file",
            "ls",
            "glob",
            "grep",
        ];
        if self.has_sandbox {
            tools.push("execute");
        }
        tools
    }

    /// Map a filesystem tool name to the operation kind for permission checks.
    ///
    /// - Read tools: `read_file`, `ls`, `glob`, `grep`
    /// - Write tools: `write_file`, `edit_file`
    /// - `execute` is not a filesystem operation → `None` (no permission check)
    /// - Unknown tools → `None` (no permission check, allow to proceed)
    fn map_tool_to_operation(tool_name: &str) -> Option<FilesystemOperation> {
        match tool_name {
            "read_file" | "ls" | "glob" | "grep" => Some(FilesystemOperation::Read),
            "write_file" | "edit_file" => Some(FilesystemOperation::Write),
            _ => None,
        }
    }

    /// Extract the filesystem path from a tool call's JSON arguments.
    ///
    /// Looks for common field names: `"path"`, `"file_path"`, `"filepath"`.
    /// Returns `None` if the JSON is invalid or none of the fields are present.
    fn extract_path(args: &str) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(args).ok()?;
        for key in &["path", "file_path", "filepath"] {
            if let Some(path) = value.get(key).and_then(|v| v.as_str()) {
                return Some(path.to_string());
            }
        }
        None
    }
}

impl std::fmt::Debug for FilesystemMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesystemMiddleware")
            .field("has_sandbox", &self.has_sandbox)
            .field("eviction_threshold", &self.eviction_threshold)
            .field("permission_count", &self.permissions.len())
            .finish()
    }
}

impl AgentHook for FilesystemMiddleware {
    fn on_model_select(
        &self,
        _ctx: &HookContext,
        _event: ModelSelection<'_>,
    ) -> ModelSelectionAction {
        ModelSelectionAction::Continue
    }

    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> impl std::future::Future<Output = CompletionCallAction> + WasmCompatSend {
        // Inject filesystem instructions via preamble patch.
        //
        // v0: Only the preamble is patched. `active_tools` is omitted because
        // filesystem tools (write_file, read_file, etc.) are not yet
        // registered as rig tool implementations — they exist only as names
        // in the system prompt. Registering concrete tool implementations
        // backed by the Backend trait is planned for v1.
        let patch = RequestPatch::new().preamble(self.fs_system_prompt());
        async move { CompletionCallAction::patch(patch) }
    }

    fn on_completion_response(
        &self,
        _ctx: &HookContext,
        _event: CompletionResponseEvent<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        _event: ModelTurnFinished<'_>,
    ) -> impl std::future::Future<Output = ModelTurnAction> + WasmCompatSend {
        async { ModelTurnAction::Continue }
    }

    fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        _event: &rig_agent::agent::InvalidToolCallContext,
    ) -> impl std::future::Future<Output = Option<InvalidToolCallAction>> + WasmCompatSend {
        async { None }
    }

    fn on_tool_call(
        &self,
        _ctx: &HookContext,
        event: RigToolCall<'_>,
    ) -> impl std::future::Future<Output = ToolCallAction> + WasmCompatSend {
        // Enforce filesystem permissions before the tool executes.
        //
        // The tool call's `args` field is a JSON string; we extract the
        // "path" (or "file_path") field, map the tool name to a
        // FilesystemOperation, then ask the PermissionChecker for a mode.
        //
        // - Allow → Run the tool normally.
        // - Deny  → Skip the tool call, sending a permission-denied message
        //           back to the model so it can adjust.
        // - Interrupt → v0: same as Deny but with a distinct message.
        //   (A future deepagents-sessions runner wrapper will convert
        //   Interrupt into a true pause/resume checkpoint.)
        let tool_name = event.tool_name.to_string();
        let args = event.args.to_string();

        let op = Self::map_tool_to_operation(&tool_name);
        let path = Self::extract_path(&args);
        let mode = match (op, path.as_deref()) {
            (Some(op), Some(path)) => self.permission_checker.check(op, path),
            // Unknown tool or no path field → allow (let the tool run).
            _ => PermissionMode::Allow,
        };

        async move {
            match mode {
                PermissionMode::Allow => ToolCallAction::Run,
                PermissionMode::Deny => {
                    let p = path.as_deref().unwrap_or("(unknown)");
                    ToolCallAction::skip(format!(
                        "Permission denied: operation on '{p}' is not allowed \
                         by the configured filesystem permissions."
                    ))
                }
                // v0: Interrupt behaves like Deny (skip with a message).
                // A future runner wrapper will intercept Interrupt to pause
                // and wait for human approval before resuming.
                PermissionMode::Interrupt => {
                    let p = path.as_deref().unwrap_or("(unknown)");
                    ToolCallAction::skip(format!(
                        "Permission interrupt: operation on '{p}' requires \
                         human approval. In v0, this is treated as a deny. \
                         (True pause/resume arrives with deepagents-sessions.)"
                    ))
                }
            }
        }
    }

    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> impl std::future::Future<Output = ToolResultAction> + WasmCompatSend {
        // Evict large results to a file, replacing the presentation with
        // a pointer to the evicted file.
        let presentation_text = event.presentation.as_text().unwrap_or("").to_string();
        let len = presentation_text.len();
        let threshold = self.eviction_threshold;
        let backend = self.backend.clone();
        let evicted_path = format!("/tmp/tool_output_{}.txt", event.internal_call_id);

        async move {
            if len > threshold {
                let _ = backend.write(&evicted_path, &presentation_text).await;
                ToolResultAction::rewrite(format!(
                    "Output evicted to {evicted_path} ({len} bytes). \
                     Use read_file to access it."
                ))
            } else {
                ToolResultAction::Keep
            }
        }
    }

    fn on_text_delta(
        &self,
        _ctx: &HookContext,
        _event: TextDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_reasoning_delta(
        &self,
        _ctx: &HookContext,
        _event: ReasoningDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_tool_call_delta(
        &self,
        _ctx: &HookContext,
        _event: ToolCallDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_stream_response_finish(
        &self,
        _ctx: &HookContext,
        _event: StreamResponseFinish<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }
}

// ── SubAgentMiddleware ─────────────────────────────────────────────────

/// The sub-agent middleware: contributes the `task` tool and injects
/// sub-agent usage instructions into the completion call.
///
/// Mapping (SPEC Q5):
/// - `on_completion_call`: inject subagent usage instructions
pub struct SubAgentMiddleware {
    /// The sub-agent specs (for documentation in the system prompt).
    subagent_names: Vec<String>,
}

impl SubAgentMiddleware {
    /// Create a new sub-agent middleware with the given sub-agent names.
    pub fn new(subagent_names: Vec<String>) -> Self {
        Self { subagent_names }
    }

    /// Create an empty sub-agent middleware.
    pub fn empty() -> Self {
        Self::new(vec![])
    }

    /// The sub-agent usage instructions injected into completion calls.
    pub fn subagent_prompt(&self) -> String {
        if self.subagent_names.is_empty() {
            return String::new();
        }
        let names = self.subagent_names.join(", ");
        format!(
            "You can delegate tasks to sub-agents using the `task` tool. \
             Available sub-agents: {names}. Use sub-agents for complex \
             subtasks that benefit from isolation."
        )
    }

    /// The list of tool names this middleware contributes.
    pub fn tool_names(&self) -> Vec<&'static str> {
        vec!["task"]
    }
}

impl std::fmt::Debug for SubAgentMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubAgentMiddleware")
            .field("subagent_count", &self.subagent_names.len())
            .finish()
    }
}

impl Default for SubAgentMiddleware {
    fn default() -> Self {
        Self::empty()
    }
}

impl AgentHook for SubAgentMiddleware {
    fn on_model_select(
        &self,
        _ctx: &HookContext,
        _event: ModelSelection<'_>,
    ) -> ModelSelectionAction {
        ModelSelectionAction::Continue
    }

    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> impl std::future::Future<Output = CompletionCallAction> + WasmCompatSend {
        let prompt = self.subagent_prompt();
        async move {
            if prompt.is_empty() {
                CompletionCallAction::Continue
            } else {
                CompletionCallAction::patch(RequestPatch::new().preamble(prompt))
            }
        }
    }

    fn on_completion_response(
        &self,
        _ctx: &HookContext,
        _event: CompletionResponseEvent<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        _event: ModelTurnFinished<'_>,
    ) -> impl std::future::Future<Output = ModelTurnAction> + WasmCompatSend {
        async { ModelTurnAction::Continue }
    }

    fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        _event: &rig_agent::agent::InvalidToolCallContext,
    ) -> impl std::future::Future<Output = Option<InvalidToolCallAction>> + WasmCompatSend {
        async { None }
    }

    fn on_tool_call(
        &self,
        _ctx: &HookContext,
        _event: RigToolCall<'_>,
    ) -> impl std::future::Future<Output = ToolCallAction> + WasmCompatSend {
        async { ToolCallAction::Run }
    }

    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        _event: ToolResultEvent<'_>,
    ) -> impl std::future::Future<Output = ToolResultAction> + WasmCompatSend {
        async { ToolResultAction::Keep }
    }

    fn on_text_delta(
        &self,
        _ctx: &HookContext,
        _event: TextDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_reasoning_delta(
        &self,
        _ctx: &HookContext,
        _event: ReasoningDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_tool_call_delta(
        &self,
        _ctx: &HookContext,
        _event: ToolCallDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_stream_response_finish(
        &self,
        _ctx: &HookContext,
        _event: StreamResponseFinish<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }
}

// ── SummarizationMiddleware ────────────────────────────────────────────

/// The summarization middleware: truncates messages and history to stay
/// within context limits. Offloads old conversation history to keep the
/// context window manageable.
///
/// Mapping (SPEC Q5):
/// - `on_completion_call`: truncate message history / argument truncation
/// - `on_tool_result`: truncate large tool results
pub struct SummarizationMiddleware {
    /// Maximum number of messages to keep in the context window.
    /// Older messages are truncated. Default: 20.
    max_messages: usize,
    /// Maximum length (in chars) of any single tool result in the history.
    /// Longer results are truncated. Default: 5000.
    max_tool_result_chars: usize,
}

impl SummarizationMiddleware {
    /// Create a new summarization middleware with defaults.
    pub fn new() -> Self {
        Self {
            max_messages: 20,
            max_tool_result_chars: 5000,
        }
    }

    /// Set the maximum number of messages to keep.
    pub fn with_max_messages(mut self, max: usize) -> Self {
        self.max_messages = max;
        self
    }

    /// Set the maximum tool result character length.
    pub fn with_max_tool_result_chars(mut self, max: usize) -> Self {
        self.max_tool_result_chars = max;
        self
    }

    /// Truncate a message history to the last `max_messages` messages.
    /// The system prompt (first message) is always preserved.
    pub fn truncate_history(&self, history: &[Message]) -> Vec<Message> {
        if history.len() <= self.max_messages {
            return history.to_vec();
        }
        // Preserve the first message (system prompt) + last N-1 messages
        let mut result = vec![history[0].clone()];
        let start = history.len() - (self.max_messages - 1);
        result.extend(history[start..].iter().cloned());
        result
    }
}

impl std::fmt::Debug for SummarizationMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummarizationMiddleware")
            .field("max_messages", &self.max_messages)
            .field("max_tool_result_chars", &self.max_tool_result_chars)
            .finish()
    }
}

impl Default for SummarizationMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentHook for SummarizationMiddleware {
    fn on_model_select(
        &self,
        _ctx: &HookContext,
        _event: ModelSelection<'_>,
    ) -> ModelSelectionAction {
        ModelSelectionAction::Continue
    }

    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> impl std::future::Future<Output = CompletionCallAction> + WasmCompatSend {
        // Truncate history if it exceeds max_messages.
        let history = event.history;
        let truncated = self.truncate_history(history);
        let needs_patch = truncated.len() != history.len();

        async move {
            if needs_patch {
                CompletionCallAction::patch(RequestPatch::new().history(truncated))
            } else {
                CompletionCallAction::Continue
            }
        }
    }

    fn on_completion_response(
        &self,
        _ctx: &HookContext,
        _event: CompletionResponseEvent<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        _event: ModelTurnFinished<'_>,
    ) -> impl std::future::Future<Output = ModelTurnAction> + WasmCompatSend {
        async { ModelTurnAction::Continue }
    }

    fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        _event: &rig_agent::agent::InvalidToolCallContext,
    ) -> impl std::future::Future<Output = Option<InvalidToolCallAction>> + WasmCompatSend {
        async { None }
    }

    fn on_tool_call(
        &self,
        _ctx: &HookContext,
        _event: RigToolCall<'_>,
    ) -> impl std::future::Future<Output = ToolCallAction> + WasmCompatSend {
        async { ToolCallAction::Run }
    }

    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> impl std::future::Future<Output = ToolResultAction> + WasmCompatSend {
        // Truncate large tool results.
        let max_chars = self.max_tool_result_chars;
        let text = event.presentation.as_text().map(|s| s.to_string());

        async move {
            if let Some(text) = text {
                if text.len() > max_chars {
                    let safe_end = max_chars.min(text.len());
                    let truncated = format!(
                        "{}\n... (truncated, {} chars total)",
                        &text[..safe_end],
                        text.len()
                    );
                    return ToolResultAction::rewrite(truncated);
                }
            }
            ToolResultAction::Keep
        }
    }

    fn on_text_delta(
        &self,
        _ctx: &HookContext,
        _event: TextDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_reasoning_delta(
        &self,
        _ctx: &HookContext,
        _event: ReasoningDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_tool_call_delta(
        &self,
        _ctx: &HookContext,
        _event: ToolCallDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }

    fn on_stream_response_finish(
        &self,
        _ctx: &HookContext,
        _event: StreamResponseFinish<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + WasmCompatSend {
        async { ObservationAction::Continue }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::StateBackend;

    #[test]
    fn test_filesystem_middleware_tool_names() {
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let mw = FilesystemMiddleware::permissive(backend);
        let names = mw.tool_names();
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"execute")); // not a sandbox by default
    }

    #[test]
    fn test_filesystem_middleware_with_sandbox() {
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let mw = FilesystemMiddleware::permissive(backend).with_sandbox(true);
        let names = mw.tool_names();
        assert!(names.contains(&"execute"));
    }

    #[test]
    fn test_filesystem_middleware_fs_prompt() {
        let backend = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let mw = FilesystemMiddleware::permissive(backend);
        let prompt = mw.fs_system_prompt();
        assert!(prompt.contains("virtual filesystem"));
    }

    #[test]
    fn test_subagent_middleware_prompt() {
        let mw = SubAgentMiddleware::new(vec!["researcher".into(), "coder".into()]);
        let prompt = mw.subagent_prompt();
        assert!(prompt.contains("researcher"));
        assert!(prompt.contains("coder"));
        assert!(prompt.contains("task"));
    }

    #[test]
    fn test_subagent_middleware_empty_prompt() {
        let mw = SubAgentMiddleware::empty();
        assert!(mw.subagent_prompt().is_empty());
    }

    #[test]
    fn test_summarization_truncate_history() {
        let mw = SummarizationMiddleware::new().with_max_messages(5);
        // Create 10 dummy messages
        let history: Vec<Message> = (0..10)
            .map(|i| Message::user(format!("msg{i}")))
            .collect();
        let truncated = mw.truncate_history(&history);
        assert_eq!(truncated.len(), 5);
        // First message preserved + last 4
        assert_eq!(truncated[0].rag_text().unwrap(), "msg0");
        assert_eq!(truncated[4].rag_text().unwrap(), "msg9");
    }

    #[test]
    fn test_summarization_no_truncation_needed() {
        let mw = SummarizationMiddleware::new().with_max_messages(20);
        let history: Vec<Message> = (0..5)
            .map(|i| Message::user(format!("msg{i}")))
            .collect();
        let truncated = mw.truncate_history(&history);
        assert_eq!(truncated.len(), 5);
    }
}

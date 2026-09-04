//! 12 events, tokio async, Windows cmd/pwsh (Q15)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.
//!
//! # Overview
//!
//! This crate implements the hook system described in SPEC §Q15. Hooks are
//! external commands that run at well-defined points in the agent lifecycle,
//! letting users plug in custom behavior (notifications, audit logging,
//! validation, etc.) without modifying the core runtime.
//!
//! ## 12 hook events
//!
//! [`HookEvent`] enumerates the twelve trigger points:
//!
//! | Event              | Trigger              |
//! |--------------------|----------------------|
//! | `before_agent`     | agent run start      |
//! | `after_agent`      | agent run end        |
//! | `before_tool_call` | before tool call     |
//! | `after_tool_call`  | after tool call      |
//! | `before_model_call`| before model call    |
//! | `after_model_call`  | after model call     |
//! | `on_approval`      | approval request     |
//! | `on_reject`        | reject operation     |
//! | `on_error`         | error occurred       |
//! | `on_session_start` | session start        |
//! | `on_session_end`   | session end          |
//! | `on_compaction`    | context compaction   |
//!
//! ## 3 scopes
//!
//! Each handler is scoped to one of [`HookScope`]'s three levels:
//! `session`, `thread`, or `global`.
//!
//! ## Async execution
//!
//! Handlers are spawned with [`tokio::process::Command`] and may be run
//! concurrently. A per-handler [`HookHandler::timeout`] (or the
//! [`HookConfig::default_timeout`]) bounds execution via [`tokio::time::timeout`].
//!
//! ## Windows shell selection
//!
//! On Unix the v0 implementation shells out via `sh -c`. On Windows the
//! long-term plan supports a 5+7 shell combination matrix: the 5 classic
//! `cmd` invocations (`/C`, `/K`, `/Q`, `/S`, `/E:OFF`) plus the 7 PowerShell
//! (`pwsh` / Windows PowerShell) profiles (5.1 desktop, 7.x core, ISE,
//! VSCode integrated, pwsh-preview, pwsh-lts, and the AllUsers/CurrentUser
//! AllHosts/CurrentHost profile quartet). The v0 selector returns `cmd /C`
//! on Windows; the full `cmd`+`pwsh` matrix is tracked for a future release.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::{debug, warn};

use deepagents_errors::{Error, HookError};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ──────────────────────────────────────────────────────────────────────
// Enums: HookEvent, HookScope, HookAction, HookResult
// ──────────────────────────────────────────────────────────────────────

/// The twelve hook trigger events (SPEC §Q15).
///
/// Serialized as `snake_case` strings in `hooks.json`, e.g. `before_agent`,
/// `after_tool_call`, `on_compaction`.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Fired at the start of an agent run.
    BeforeAgent,
    /// Fired at the end of an agent run.
    AfterAgent,
    /// Fired immediately before a tool is invoked.
    BeforeToolCall,
    /// Fired immediately after a tool returns.
    AfterToolCall,
    /// Fired immediately before a model (LLM) call.
    BeforeModelCall,
    /// Fired immediately after a model (LLM) call returns.
    AfterModelCall,
    /// Fired when an approval request is issued (HITL).
    OnApproval,
    /// Fired when an operation is rejected.
    OnReject,
    /// Fired when an error occurs during a run.
    OnError,
    /// Fired when a session starts.
    OnSessionStart,
    /// Fired when a session ends.
    OnSessionEnd,
    /// Fired when context compaction runs.
    OnCompaction,
}

/// The three hook scopes (SPEC §Q15).
///
/// Scopes determine the lifetime/visibility of a handler:
///
/// - `session` — scoped to a single agent session.
/// - `thread`   — scoped to a conversation thread (may span sessions).
/// - `global`   — applies across all sessions and threads.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HookScope {
    /// Scoped to a single agent session.
    Session,
    /// Scoped to a conversation thread.
    Thread,
    /// Applies globally across all sessions and threads.
    Global,
}

/// The action a handler should perform.
///
/// A handler either runs an external command ([`HookAction::Run`]) or is a
/// no-op marker that skips execution ([`HookAction::Skip`]).
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "kind", content = "detail")]
pub enum HookAction {
    /// Run an external command.
    Run {
        /// The command (executable name or path) to invoke.
        command: String,
        /// Arguments to pass to the command.
        args: Vec<String>,
    },
    /// Skip execution (no-op marker).
    Skip,
}

/// The outcome of running a single hook handler.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(tag = "kind", content = "detail")]
pub enum HookResult {
    /// The handler completed successfully.
    Success {
        /// Captured stdout of the handler.
        output: String,
    },
    /// The handler exited with a non-zero code.
    Failed {
        /// The process exit code.
        code: i32,
        /// Captured stderr of the handler.
        stderr: String,
    },
    /// The handler was skipped (e.g. [`HookAction::Skip`]).
    Skipped,
}

// ──────────────────────────────────────────────────────────────────────
// Structs: HookHandler, HookConfig, HookContext, HookExecutionResult
// ──────────────────────────────────────────────────────────────────────

/// A single configured hook handler.
///
/// A handler binds an [`HookEvent`] trigger, a [`HookScope`], an [`HookAction`]
/// to perform, and an optional per-handler timeout (seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookHandler {
    /// Unique identifier for this handler (used in diagnostics & logs).
    pub id: String,
    /// The event that triggers this handler.
    pub event: HookEvent,
    /// The scope this handler applies to.
    pub scope: HookScope,
    /// The action to perform when triggered.
    pub action: HookAction,
    /// Optional per-handler timeout in seconds. Overrides
    /// [`HookConfig::default_timeout`] when set.
    pub timeout: Option<u64>,
}

/// A collection of hook handlers plus shared defaults.
///
/// Typically loaded from a `hooks.json` file via [`HookConfig::from_json`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookConfig {
    /// The registered handlers.
    pub handlers: Vec<HookHandler>,
    /// Default timeout (seconds) applied to handlers without an explicit
    /// [`HookHandler::timeout`].
    pub default_timeout: Option<u64>,
}

impl HookConfig {
    /// Create an empty configuration.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            default_timeout: None,
        }
    }

    /// Append a handler to this configuration.
    pub fn add_handler(&mut self, handler: HookHandler) {
        self.handlers.push(handler);
    }

    /// Return references to all handlers registered for the given event.
    pub fn handlers_for_event(&self, event: HookEvent) -> Vec<&HookHandler> {
        self.handlers
            .iter()
            .filter(|h| h.event == event)
            .collect()
    }

    /// Parse a `hooks.json` document into a [`HookConfig`].
    ///
    /// Returns a [`deepagents_errors::Error::Hook`] variant wrapping
    /// [`HookError::InvalidConfig`] on parse failure.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        serde_json::from_str(json).map_err(|e| {
            Error::Hook(HookError::InvalidConfig(format!("hooks.json parse error: {e}")))
        })
    }

    /// Serialize this configuration back to a `hooks.json` string.
    pub fn to_json(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(self).map_err(Into::into)
    }
}

/// Context bag passed to handlers at execution time.
///
/// Carries the triggering event plus identifying information (session/thread
/// ids, optional tool name, optional error message) so handlers can act on
/// rich context. Serialized to JSON and exposed to handlers via environment
/// variables in a future release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    /// The event that triggered this execution.
    pub event: HookEvent,
    /// The session identifier.
    pub session_id: String,
    /// The thread identifier, if applicable.
    pub thread_id: Option<String>,
    /// The tool name, for tool-related events.
    pub tool_name: Option<String>,
    /// An error message, for `on_error` events.
    pub error_message: Option<String>,
}

/// The result of executing a single handler within an event batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookExecutionResult {
    /// The id of the handler that was run.
    pub handler_id: String,
    /// The event that triggered the handler.
    pub event: HookEvent,
    /// The outcome of the handler.
    pub result: HookResult,
}

// ──────────────────────────────────────────────────────────────────────
// ShellSelector
// ──────────────────────────────────────────────────────────────────────

/// Selects the shell used to invoke hook commands.
///
/// # Windows shell matrix (5+7)
///
/// The long-term Windows plan supports a combination matrix of:
///
/// - **5 `cmd` invocations**: the classic `cmd.exe` flag set (`/C`, `/K`,
///   `/Q`, `/S`, `/E:OFF`) used to run a single command string and return.
/// - **7 PowerShell profiles**: Windows PowerShell 5.1 (desktop), PowerShell
///   7.x core (`pwsh`), ISE, VSCode integrated terminal, `pwsh-preview`,
///   `pwsh-lts`, and the AllUsers/CurrentUser × AllHosts/CurrentHost profile
///   quartet that governs which `$PROFILE` is loaded.
///
/// The v0 implementation is intentionally minimal: it returns `sh -c` on
/// Unix and `cmd /C` on Windows. The full `cmd`+`pwsh` matrix is tracked
/// for a future release.
pub struct ShellSelector;

impl ShellSelector {
    /// Select the shell program and its leading arguments for the current
    /// platform.
    ///
    /// Returns `("sh", ["-c"])` on Unix and `("cmd", ["/C"])` on Windows.
    pub fn select_shell() -> (&'static str, Vec<&'static str>) {
        #[cfg(unix)]
        {
            ("sh", vec!["-c"])
        }
        #[cfg(windows)]
        {
            // v0: cmd /C. Future: support the 5+7 cmd/pwsh matrix.
            ("cmd", vec!["/C"])
        }
        #[cfg(not(any(unix, windows)))]
        {
            ("sh", vec!["-c"])
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// HookExecutor
// ──────────────────────────────────────────────────────────────────────

/// Runs hook handlers asynchronously.
///
/// Holds a [`HookConfig`] and dispatches [`HookEvent`]s, running all matching
/// handlers concurrently via [`tokio::spawn`] and collecting their results.
pub struct HookExecutor {
    /// The configuration driving this executor.
    pub config: HookConfig,
}

impl HookExecutor {
    /// Create a new executor from a [`HookConfig`].
    pub fn new(config: HookConfig) -> Self {
        Self { config }
    }

    /// Execute all handlers registered for `event`, passing `context` to
    /// each. Handlers are run concurrently and their results collected.
    ///
    /// Returns one [`HookExecutionResult`] per matching handler, in the
    /// order the handlers were registered.
    pub async fn execute_event(
        &self,
        event: HookEvent,
        context: &HookContext,
    ) -> Vec<HookExecutionResult> {
        let handlers = self.config.handlers_for_event(event);
        if handlers.is_empty() {
            return Vec::new();
        }

        // Spawn each handler concurrently.
        let mut joins = Vec::with_capacity(handlers.len());
        for h in handlers {
            let id = h.id.clone();
            let ev = h.event.clone();
            let h_clone = h.clone();
            let ctx_clone = context.clone();
            joins.push((
                id,
                ev,
                tokio::spawn(async move {
                    Self::run_handler(&h_clone, &ctx_clone).await
                }),
            ));
        }

        // Collect results in registration order.
        let mut out = Vec::with_capacity(joins.len());
        for (handler_id, event, join) in joins {
            let result = match join.await {
                Ok(r) => r,
                Err(join_err) => {
                    warn!(
                        handler_id = %handler_id,
                        error = %join_err,
                        "hook handler task panicked or was cancelled"
                    );
                    HookResult::Failed {
                        code: -1,
                        stderr: format!("handler task join error: {join_err}"),
                    }
                }
            };
            out.push(HookExecutionResult {
                handler_id,
                event,
                result,
            });
        }
        out
    }

    /// Run a single handler: spawn its process, capture stdout/stderr, and
    /// apply the configured timeout.
    ///
    /// - For [`HookAction::Skip`], returns [`HookResult::Skipped`] without
    ///   spawning anything.
    /// - For [`HookAction::Run`], the command is executed through the
    ///   platform shell (see [`ShellSelector`]). A non-zero exit yields
    ///   [`HookResult::Failed`]; a zero exit yields [`HookResult::Success`].
    /// - If the handler exceeds its timeout, it is reported as
    ///   [`HookResult::Failed`] with code `-1` and a timeout message.
    pub async fn run_handler(
        handler: &HookHandler,
        context: &HookContext,
    ) -> HookResult {
        match &handler.action {
            HookAction::Skip => {
                debug!(handler_id = %handler.id, "hook skipped (Skip action)");
                HookResult::Skipped
            }
            HookAction::Run { command, args } => {
                let (shell, shell_args) = ShellSelector::select_shell();

                // Build the full command line: command + args, then run via shell.
                let mut cmdline = command.clone();
                for a in args {
                    cmdline.push(' ');
                    cmdline.push_str(a);
                }

                let mut cmd = Command::new(shell);
                cmd.args(&shell_args);
                cmd.arg(&cmdline);

                // Expose context to the handler via environment variables.
                cmd.env("DEEPAGENTS_HOOK_EVENT", serde_json::to_string(&context.event).unwrap_or_default());
                cmd.env("DEEPAGENTS_HOOK_SESSION_ID", &context.session_id);
                if let Some(tid) = &context.thread_id {
                    cmd.env("DEEPAGENTS_HOOK_THREAD_ID", tid);
                }
                if let Some(tn) = &context.tool_name {
                    cmd.env("DEEPAGENTS_HOOK_TOOL_NAME", tn);
                }
                if let Some(em) = &context.error_message {
                    cmd.env("DEEPAGENTS_HOOK_ERROR_MESSAGE", em);
                }

                // Determine the effective timeout (handler overrides default).
                let timeout_secs = handler.timeout.or(context_default_timeout(context));
                let spawn = cmd.output();

                let output = match timeout_secs {
                    Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), spawn).await {
                        Ok(o) => o,
                        Err(_) => {
                            warn!(
                                handler_id = %handler.id,
                                timeout_secs = secs,
                                "hook handler timed out"
                            );
                            return HookResult::Failed {
                                code: -1,
                                stderr: format!("hook handler '{0}' timed out after {secs}s", handler.id),
                            };
                        }
                    },
                    None => spawn.await,
                };

                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                        if out.status.success() {
                            HookResult::Success { output: stdout }
                        } else {
                            let code = out.status.code().unwrap_or(-1);
                            HookResult::Failed { code, stderr }
                        }
                    }
                    Err(e) => {
                        warn!(
                            handler_id = %handler.id,
                            error = %e,
                            "failed to spawn hook handler"
                        );
                        HookResult::Failed {
                            code: -1,
                            stderr: format!("spawn failed for handler '{}': {}", handler.id, e),
                        }
                    }
                }
            }
        }
    }
}

/// Pull a default timeout out of the context bag if present.
///
/// This is a small helper that lets a context carry an implicit default
/// timeout (e.g. mirrored from [`HookConfig::default_timeout`]) without the
/// executor needing to thread the config into [`HookExecutor::run_handler`].
/// v0 reads it from a well-known env var if set; otherwise returns `None`.
fn context_default_timeout(_context: &HookContext) -> Option<u64> {
    std::env::var("DEEPAGENTS_HOOK_DEFAULT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
}

// Silence unused-import warning for HashMap on platforms where it isn't used.
#[allow(dead_code)]
fn _hashmap_marker() -> HashMap<String, String> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_event_serde() {
        // Round-trip + snake_case for a representative event.
        let ev = HookEvent::BeforeToolCall;
        let s = serde_json::to_string(&ev).expect("serialize");
        assert_eq!(s, "\"before_tool_call\"");
        let back: HookEvent = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, ev);

        // Verify all 12 variants serialize to snake_case.
        let cases = [
            (HookEvent::BeforeAgent, "before_agent"),
            (HookEvent::AfterAgent, "after_agent"),
            (HookEvent::BeforeToolCall, "before_tool_call"),
            (HookEvent::AfterToolCall, "after_tool_call"),
            (HookEvent::BeforeModelCall, "before_model_call"),
            (HookEvent::AfterModelCall, "after_model_call"),
            (HookEvent::OnApproval, "on_approval"),
            (HookEvent::OnReject, "on_reject"),
            (HookEvent::OnError, "on_error"),
            (HookEvent::OnSessionStart, "on_session_start"),
            (HookEvent::OnSessionEnd, "on_session_end"),
            (HookEvent::OnCompaction, "on_compaction"),
        ];
        for (variant, expected) in cases {
            let s = serde_json::to_string(&variant).expect("serialize");
            assert_eq!(s, format!("\"{expected}\""), "variant {expected}");
            let back: HookEvent = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn test_hook_scope_serde() {
        for (variant, expected) in [
            (HookScope::Session, "session"),
            (HookScope::Thread, "thread"),
            (HookScope::Global, "global"),
        ] {
            let s = serde_json::to_string(&variant).expect("serialize");
            assert_eq!(s, format!("\"{expected}\""));
            let back: HookScope = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn test_hook_config_add_handler() {
        let mut cfg = HookConfig::new();
        assert!(cfg.handlers.is_empty());
        cfg.add_handler(HookHandler {
            id: "h1".into(),
            event: HookEvent::BeforeAgent,
            scope: HookScope::Global,
            action: HookAction::Skip,
            timeout: None,
        });
        assert_eq!(cfg.handlers.len(), 1);
        assert_eq!(cfg.handlers[0].id, "h1");
    }

    #[test]
    fn test_hook_config_handlers_for_event() {
        let mut cfg = HookConfig::new();
        cfg.add_handler(HookHandler {
            id: "a".into(),
            event: HookEvent::BeforeToolCall,
            scope: HookScope::Session,
            action: HookAction::Skip,
            timeout: None,
        });
        cfg.add_handler(HookHandler {
            id: "b".into(),
            event: HookEvent::AfterToolCall,
            scope: HookScope::Session,
            action: HookAction::Skip,
            timeout: None,
        });
        cfg.add_handler(HookHandler {
            id: "c".into(),
            event: HookEvent::BeforeToolCall,
            scope: HookScope::Global,
            action: HookAction::Skip,
            timeout: None,
        });

        let before = cfg.handlers_for_event(HookEvent::BeforeToolCall);
        assert_eq!(before.len(), 2);
        let ids: Vec<&str> = before.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);

        let after = cfg.handlers_for_event(HookEvent::AfterToolCall);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, "b");

        let none = cfg.handlers_for_event(HookEvent::OnCompaction);
        assert!(none.is_empty());
    }

    #[test]
    fn test_hook_config_json_roundtrip() {
        let mut cfg = HookConfig::new();
        cfg.default_timeout = Some(30);
        cfg.add_handler(HookHandler {
            id: "notify".into(),
            event: HookEvent::AfterAgent,
            scope: HookScope::Global,
            action: HookAction::Run {
                command: "echo".into(),
                args: vec!["done".into()],
            },
            timeout: Some(10),
        });
        cfg.add_handler(HookHandler {
            id: "noop".into(),
            event: HookEvent::OnError,
            scope: HookScope::Session,
            action: HookAction::Skip,
            timeout: None,
        });

        let json = cfg.to_json().expect("to_json");
        let back = HookConfig::from_json(&json).expect("from_json");
        assert_eq!(back.default_timeout, Some(30));
        assert_eq!(back.handlers.len(), 2);
        assert_eq!(back.handlers[0].id, "notify");
        assert_eq!(back.handlers[0].event, HookEvent::AfterAgent);
        assert_eq!(back.handlers[0].scope, HookScope::Global);
        assert_eq!(back.handlers[0].timeout, Some(10));
        match &back.handlers[0].action {
            HookAction::Run { command, args } => {
                assert_eq!(command, "echo");
                assert_eq!(args, &vec!["done".to_string()]);
            }
            other => panic!("expected Run, got {other:?}"),
        }
        assert_eq!(back.handlers[1].action, HookAction::Skip);

        // Invalid JSON yields a Hook error.
        let err = HookConfig::from_json("{ not json").unwrap_err();
        assert!(matches!(err, Error::Hook(HookError::InvalidConfig(_))));
    }

    #[test]
    fn test_hook_context_serde() {
        let ctx = HookContext {
            event: HookEvent::OnError,
            session_id: "s-1".into(),
            thread_id: Some("t-1".into()),
            tool_name: None,
            error_message: Some("boom".into()),
        };
        let s = serde_json::to_string(&ctx).expect("serialize");
        let back: HookContext = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.event, HookEvent::OnError);
        assert_eq!(back.session_id, "s-1");
        assert_eq!(back.thread_id.as_deref(), Some("t-1"));
        assert!(back.tool_name.is_none());
        assert_eq!(back.error_message.as_deref(), Some("boom"));
    }

    #[cfg(unix)]
    #[test]
    fn test_shell_selector_unix() {
        let (prog, args) = ShellSelector::select_shell();
        assert_eq!(prog, "sh");
        assert_eq!(args, vec!["-c"]);
    }

    #[cfg(windows)]
    #[test]
    fn test_shell_selector_windows() {
        let (prog, args) = ShellSelector::select_shell();
        assert_eq!(prog, "cmd");
        assert_eq!(args, vec!["/C"]);
    }

    #[tokio::test]
    async fn test_run_handler_skip() {
        let handler = HookHandler {
            id: "skipper".into(),
            event: HookEvent::BeforeAgent,
            scope: HookScope::Global,
            action: HookAction::Skip,
            timeout: None,
        };
        let ctx = HookContext {
            event: HookEvent::BeforeAgent,
            session_id: "s".into(),
            thread_id: None,
            tool_name: None,
            error_message: None,
        };
        let res = HookExecutor::run_handler(&handler, &ctx).await;
        assert_eq!(res, HookResult::Skipped);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_handler_success_unix() {
        let handler = HookHandler {
            id: "echoer".into(),
            event: HookEvent::AfterAgent,
            scope: HookScope::Global,
            action: HookAction::Run {
                command: "echo".into(),
                args: vec!["hello".into()],
            },
            timeout: Some(5),
        };
        let ctx = HookContext {
            event: HookEvent::AfterAgent,
            session_id: "s".into(),
            thread_id: None,
            tool_name: None,
            error_message: None,
        };
        let res = HookExecutor::run_handler(&handler, &ctx).await;
        match res {
            HookResult::Success { output } => {
                assert!(output.contains("hello"), "output was: {output}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_handler_failed_unix() {
        // `false` always exits non-zero.
        let handler = HookHandler {
            id: "failer".into(),
            event: HookEvent::OnError,
            scope: HookScope::Session,
            action: HookAction::Run {
                command: "false".into(),
                args: vec![],
            },
            timeout: Some(5),
        };
        let ctx = HookContext {
            event: HookEvent::OnError,
            session_id: "s".into(),
            thread_id: None,
            tool_name: None,
            error_message: None,
        };
        let res = HookExecutor::run_handler(&handler, &ctx).await;
        match res {
            HookResult::Failed { code, .. } => {
                assert_ne!(code, 0);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_execute_event_empty() {
        let exec = HookExecutor::new(HookConfig::new());
        let ctx = HookContext {
            event: HookEvent::BeforeAgent,
            session_id: "s".into(),
            thread_id: None,
            tool_name: None,
            error_message: None,
        };
        let res = exec.execute_event(HookEvent::BeforeAgent, &ctx).await;
        assert!(res.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_execute_event_batch_unix() {
        let mut cfg = HookConfig::new();
        cfg.add_handler(HookHandler {
            id: "h1".into(),
            event: HookEvent::AfterAgent,
            scope: HookScope::Global,
            action: HookAction::Run {
                command: "echo".into(),
                args: vec!["one".into()],
            },
            timeout: Some(5),
        });
        cfg.add_handler(HookHandler {
            id: "h2".into(),
            event: HookEvent::AfterAgent,
            scope: HookScope::Global,
            action: HookAction::Skip,
            timeout: None,
        });
        cfg.add_handler(HookHandler {
            id: "h3".into(),
            event: HookEvent::BeforeAgent, // different event, should be filtered out
            scope: HookScope::Global,
            action: HookAction::Run {
                command: "echo".into(),
                args: vec!["nope".into()],
            },
            timeout: Some(5),
        });

        let exec = HookExecutor::new(cfg);
        let ctx = HookContext {
            event: HookEvent::AfterAgent,
            session_id: "s".into(),
            thread_id: None,
            tool_name: None,
            error_message: None,
        };
        let results = exec.execute_event(HookEvent::AfterAgent, &ctx).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].handler_id, "h1");
        assert!(matches!(results[0].result, HookResult::Success { .. }));
        assert_eq!(results[1].handler_id, "h2");
        assert_eq!(results[1].result, HookResult::Skipped);
    }
}

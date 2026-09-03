//! ~40 error types, structured diagnostics (Q26)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` §十四 for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod diagnostic;
mod domains;

pub use diagnostic::{Diagnostic, DiagnosticDomain, DiagnosticSeverity};
pub use domains::*;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Marker prefix emitted to stderr when a startup failure occurs.
///
/// Parent processes scan stderr for this marker to upgrade opaque exit codes
/// into actionable summaries.
pub const STARTUP_ERROR_MARKER: &str = "DEEPAGENTS_STARTUP_ERROR:";

/// Emit a startup failure to stderr with the marker prefix.
///
/// This is called by the binary entry point when a fatal error occurs during
/// startup, so that a parent process (e.g. a wrapper TUI) can parse the error
/// and present it to the user.
pub fn emit_startup_failure(e: &Error) {
    eprintln!("{STARTUP_ERROR_MARKER} {e}");
}

/// The top-level error enum, aggregating all domain errors.
///
/// Every domain error type implements `Into<Error>` via `#[from]`, so callers
/// can use `?` to propagate any domain error up to this enum.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Home directory resolution / access errors.
    #[error(transparent)]
    Home(#[from] HomeError),

    /// Startup / initialization errors.
    #[error(transparent)]
    Startup(#[from] StartupError),

    /// Configuration loading / validation errors.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// MCP client / server / OAuth errors.
    #[error(transparent)]
    Mcp(#[from] McpError),

    /// Approval / classifier / HITL errors.
    #[error(transparent)]
    Approval(#[from] ApprovalError),

    /// Session / resume / compaction errors.
    #[error(transparent)]
    Session(#[from] SessionError),

    /// Goal / rubric / grading errors.
    #[error(transparent)]
    Goal(#[from] GoalError),

    /// Sandbox provider errors.
    #[error(transparent)]
    Sandbox(#[from] SandboxError),

    /// Plugin / extension / marketplace errors.
    #[error(transparent)]
    Plugin(#[from] PluginError),

    /// Self-update / version check errors.
    #[error(transparent)]
    Update(#[from] UpdateError),

    /// Hook execution errors.
    #[error(transparent)]
    Hook(#[from] HookError),

    /// Tracing / LangSmith errors.
    #[error(transparent)]
    Tracing(#[from] TracingError),

    /// Context offload errors.
    #[error(transparent)]
    Offload(#[from] OffloadError),

    /// TUI / rendering errors.
    #[error(transparent)]
    Tui(#[from] TuiError),

    /// IO errors (passthrough from std).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization errors.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

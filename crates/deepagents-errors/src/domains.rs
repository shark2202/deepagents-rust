//! Domain error types (~40 types across 7+ domains, per SPEC §十四).
//!
//! Each domain has its own error enum, all convertible into the top-level
//! [`crate::Error`] via `#[from]`.

use serde::{Deserialize, Serialize};

// ── 1. Home / Startup ──────────────────────────────────────────────────

/// Errors resolving or accessing the `~/.deepagents/` home directory.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum HomeError {
    /// The `DEEPAGENTS_HOME` env var points to a path that does not exist.
    #[error("home directory not found: {0}")]
    NotFound(String),
    /// The home path exists but is not a directory.
    #[error("home path is not a directory: {0}")]
    NotADirectory(String),
    /// Permission denied when accessing the home directory.
    #[error("permission denied on home directory: {0}")]
    PermissionDenied(String),
    /// Could not create the home directory tree.
    #[error("failed to create home directory: {0}")]
    CreateFailed(String),
}

/// Startup / initialization errors (before the agent runtime begins).
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum StartupError {
    /// A required dependency or precondition is missing.
    #[error("missing dependency: {0}")]
    MissingDependency(String),
    /// A required configuration value is absent or invalid.
    #[error("invalid configuration at startup: {0}")]
    InvalidConfig(String),
    /// A required file or directory is not found at startup.
    #[error("required path not found: {0}")]
    PathNotFound(String),
    /// A generic startup failure.
    #[error("startup failed: {0}")]
    Generic(String),
}

// ── 2. Config ──────────────────────────────────────────────────────────

/// Configuration loading / validation errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum ConfigError {
    /// Managed config (admin policy) rejected a value.
    #[error("managed config error: {0}")]
    ManagedConfig(String),
    /// Managed policy denied a configuration option.
    #[error("managed policy error: {0}")]
    ManagedPolicy(String),
    /// Failed to load / parse a config.toml file.
    #[error("config load error: {0}")]
    Load(String),
    /// Model configuration is invalid.
    #[error("model config error: {0}")]
    ModelConfig(String),
    /// Unknown provider specified.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    /// Provider remote policy error.
    #[error("provider remote policy error: {0}")]
    ProviderRemotePolicy(String),
    /// Extras (profile.extra) error.
    #[error("extras error: {0}")]
    Extras(String),
    /// A configuration option was shadowed by a higher-rank source.
    #[error("option shadowed: {0}")]
    Shadowed(String),
}

// ── 3. MCP ─────────────────────────────────────────────────────────────

/// MCP client / server / OAuth errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum McpError {
    /// `.mcp.json` configuration is invalid.
    #[error("MCP config error: {0}")]
    Config(String),
    /// User cancelled an MCP login flow.
    #[error("MCP login cancelled")]
    LoginCancelled,
    /// Re-authentication is required for an MCP server.
    #[error("MCP re-auth required: {0}")]
    ReauthRequired(String),
    /// Failed to resolve an MCP server config.
    #[error("MCP config resolution error: {0}")]
    Resolution(String),
    /// Kind of MCP config error (5 sub-values).
    #[error("MCP config error kind: {0}")]
    ConfigKind(String),
    /// Transport error connecting to an MCP server.
    #[error("MCP transport error: {0}")]
    Transport(String),
}

// ── 4. Approval / Classifier ───────────────────────────────────────────

/// Approval / classifier / HITL errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum ApprovalError {
    /// The classifier did not respond within the deadline.
    #[error("classifier deadline exceeded")]
    ClassifierDeadlineExceeded,
    /// The classifier model is unavailable.
    #[error("classifier model unavailable: {0}")]
    ClassifierModelUnavailable(String),
    /// HITL iteration limit exceeded (too many approval rounds).
    #[error("HITL iteration limit exceeded: {0}")]
    HitlIterationLimit(String),
    /// A client hook requested a stop.
    #[error("client hook stop: {0}")]
    ClientHookStop(String),
    /// The hook transport was interrupted.
    #[error("hook transport interrupted: {0}")]
    HookTransportInterrupt(String),
}

// ── 5. Session / Goal ──────────────────────────────────────────────────

/// Session / resume / compaction errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum SessionError {
    /// Session database error.
    #[error("session database error: {0}")]
    Database(String),
    /// Failed to deserialize a stored session.
    #[error("session deserialize error: {0}")]
    Deserialize(String),
    /// Session not found.
    #[error("session not found: {0}")]
    NotFound(String),
    /// Auto-compaction is blocked.
    #[error("auto-compaction blocked: {0}")]
    AutoCompactionBlocked(String),
}

/// Goal / rubric / grading errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum GoalError {
    /// Goal state exceeds size limit.
    #[error("goal state size error: {0}")]
    StateSize(String),
    /// Grader returned an error.
    #[error("grader error: {0}")]
    Grader(String),
    /// Goal not found.
    #[error("goal not found: {0}")]
    NotFound(String),
}

// ── 6. Sandbox / Plugin ───────────────────────────────────────────────

/// Sandbox provider errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum SandboxError {
    /// Sandbox not found.
    #[error("sandbox not found: {0}")]
    NotFound(String),
    /// Sandbox provider error.
    #[error("sandbox error: {0}")]
    Provider(String),
    /// Sandbox creation failed.
    #[error("sandbox creation failed: {0}")]
    CreationFailed(String),
    /// Sandbox execution error.
    #[error("sandbox execution error: {0}")]
    Execution(String),
}

/// Plugin / extension / marketplace errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum PluginError {
    /// Plugin manifest is invalid.
    #[error("plugin manifest error: {0}")]
    Manifest(String),
    /// Plugin state error (enabled/disabled/marketplace).
    #[error("plugin state error: {0}")]
    State(String),
    /// Extension error (Python native, not portable).
    #[error("extension error: {0}")]
    Extension(String),
    /// Marketplace error (git clone, etc.).
    #[error("marketplace error: {0}")]
    Marketplace(String),
    /// Checksum mismatch (downloaded plugin does not match expected hash).
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Expected checksum.
        expected: String,
        /// Actual checksum.
        actual: String,
    },
}

// ── 7. Update ──────────────────────────────────────────────────────────

/// Self-update / version check errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum UpdateError {
    /// No writable binary directory found for self-update.
    #[error("no writable binary directory: {0}")]
    NoWritableBinDir(String),
    /// Failed to download an update.
    #[error("download failed: {0}")]
    DownloadFailed(String),
    /// Version check failed (network or parse error).
    #[error("version check failed: {0}")]
    VersionCheck(String),
    /// Checksum verification failed for a downloaded asset.
    #[error("checksum verification failed: {0}")]
    ChecksumFailed(String),
    /// Atomic replacement of the binary failed.
    #[error("binary replacement failed: {0}")]
    ReplaceFailed(String),
}

// ── 8. Hook ────────────────────────────────────────────────────────────

/// Hook execution errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum HookError {
    /// A hook handler returned a non-zero exit code.
    #[error("hook handler '{handler_id}' exited with code {code}: {message}")]
    HandlerExited {
        /// The handler that failed.
        handler_id: String,
        /// Exit code.
        code: i32,
        /// Error message from stderr.
        message: String,
    },
    /// Failed to spawn the hook handler process.
    #[error("hook spawn failed for handler '{handler_id}': {message}")]
    SpawnFailed {
        /// The handler that failed to spawn.
        handler_id: String,
        /// Error message.
        message: String,
    },
    /// Hook configuration is invalid.
    #[error("invalid hook config: {0}")]
    InvalidConfig(String),
}

/// A hook diagnostic (handler_id + code + message), for non-fatal warnings.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[error("hook diagnostic [{handler_id}] {code}: {message}")]
pub struct HookDiagnostic {
    /// The handler that produced this diagnostic.
    pub handler_id: String,
    /// Machine-readable code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

// ── 9. Tracing ──────────────────────────────────────────────────────────

/// Tracing / LangSmith errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum TracingError {
    /// LangSmith lookup error (general).
    #[error("langsmith lookup error: {0}")]
    Lookup(String),
    /// LangSmith import error (failed to import tracing data).
    #[error("langsmith import error: {0}")]
    Import(String),
    /// LangSmith lookup timed out.
    #[error("langsmith lookup timeout: {0}")]
    LookupTimeout(String),
    /// LangSmith API error.
    #[error("langsmith API error: {0}")]
    Api(String),
    /// LangSmith project not found.
    #[error("langsmith project not found: {0}")]
    ProjectNotFound(String),
}

// ── 10. Offload ────────────────────────────────────────────────────────

/// Context offload errors (large results evicted to files).
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum OffloadError {
    /// An offload conflict occurred (two offloads to the same key).
    #[error("offload conflict: {0}")]
    Conflict(String),
    /// Offload target is unavailable.
    #[error("offload unavailable: {0}")]
    Unavailable(String),
    /// Offload result is indeterminate (cannot determine success/failure).
    #[error("offload indeterminate: {0}")]
    Indeterminate(String),
}

// ── 11. TUI ────────────────────────────────────────────────────────────

/// TUI / rendering errors.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
pub enum TuiError {
    /// A TUI application error (rendering, state, etc.).
    #[error("TUI app error: {0}")]
    App(String),
    /// External editor invocation failed.
    #[error("external editor error: {0}")]
    ExternalEditor(String),
    /// Terminal initialization error.
    #[error("terminal init error: {0}")]
    TerminalInit(String),
}

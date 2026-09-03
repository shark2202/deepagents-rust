//! Structured diagnostics for the `doctor` command and runtime warnings.

use serde::{Deserialize, Serialize};

/// Severity level for a diagnostic entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    /// Error-level: something is broken.
    Error,
    /// Warning-level: something is degraded but functional.
    Warning,
    /// Info-level: informational note.
    Info,
}

impl std::fmt::Display for DiagnosticSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => write!(f, "error"),
            Self::Warning => write!(f, "warning"),
            Self::Info => write!(f, "info"),
        }
    }
}

/// The domain a diagnostic belongs to.
///
/// Matches the 7 error domains from SPEC §十四.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticDomain {
    /// Startup / home directory.
    Startup,
    /// Configuration loading / validation.
    Config,
    /// MCP client / server / OAuth.
    Mcp,
    /// Approval / classifier / HITL.
    Approval,
    /// Session / resume / compaction.
    Session,
    /// Sandbox / plugin.
    Sandbox,
    /// Update / version check.
    Update,
    /// Hook execution.
    Hook,
    /// Tracing / LangSmith.
    Tracing,
    /// Context offload.
    Offload,
    /// TUI rendering.
    Tui,
}

impl std::fmt::Display for DiagnosticDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl DiagnosticDomain {
    /// Return the string identifier for this domain.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Config => "config",
            Self::Mcp => "mcp",
            Self::Approval => "approval",
            Self::Session => "session",
            Self::Sandbox => "sandbox",
            Self::Update => "update",
            Self::Hook => "hook",
            Self::Tracing => "tracing",
            Self::Offload => "offload",
            Self::Tui => "tui",
        }
    }
}

/// A single diagnostic entry.
///
/// Used by the `doctor` command (Q25) and by runtime warning collectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Severity: error / warning / info.
    pub severity: DiagnosticSeverity,
    /// The domain this diagnostic belongs to.
    pub domain: DiagnosticDomain,
    /// Machine-readable error code (e.g. `"config.load.failed"`).
    pub code: String,
    /// Human-readable summary message.
    pub message: String,
    /// Source file path or component that produced this diagnostic, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Longer detail / context, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Diagnostic {
    /// Create a new error-level diagnostic.
    pub fn error(domain: DiagnosticDomain, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            domain,
            code: code.into(),
            message: message.into(),
            source: None,
            detail: None,
        }
    }

    /// Create a new warning-level diagnostic.
    pub fn warning(domain: DiagnosticDomain, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            domain,
            code: code.into(),
            message: message.into(),
            source: None,
            detail: None,
        }
    }

    /// Create a new info-level diagnostic.
    pub fn info(domain: DiagnosticDomain, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Info,
            domain,
            code: code.into(),
            message: message.into(),
            source: None,
            detail: None,
        }
    }

    /// Attach a source to this diagnostic.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Attach detail to this diagnostic.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.code, self.message)?;
        if let Some(src) = &self.source {
            write!(f, " (source: {src})")?;
        }
        Ok(())
    }
}

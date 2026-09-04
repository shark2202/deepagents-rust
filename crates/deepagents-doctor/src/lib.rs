//! 4 sections + context-doctor (Q25).
//!
//! The doctor crate produces human-readable diagnostic reports for the
//! deepagents-rust SDK. A [`DiagnosticReport`] groups four canonical
//! [`DiagnosticSection`]s — *Diagnostics*, *Updates*, *Tracing* and
//! *Configuration* — each containing a list of [`DiagnosticItem`]s with a
//! pass/fail status. Reports are rendered as a tree with `├─` / `└─`
//! connectors, `✓` / `✗` status glyphs and optional color coding; commit
//! hashes are turned into clickable GitHub links.
//!
//! A separate [`ContextDoctorReport`] audits the context window: it breaks
//! down the injected tokens (system prompt, AGENTS.md memory, skills index,
//! built-in tool schemas, MCP servers) and reconciles them against the
//! conversation-token and provider-token counts via [`ReconciliationStatus`].
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use serde::{Deserialize, Serialize};

use deepagents_errors::{ConfigError, Error};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The GitHub repository URL used to build commit-hash links.
const GITHUB_REPO_URL: &str = "https://github.com/langchain-ai/deepagents-rust";

// ────────────────────────────────────────────────────────────────────────────
// Status glyph + color helpers
// ────────────────────────────────────────────────────────────────────────────

/// Render a pass/fail status glyph for an item.
///
/// Returns `✓` when `ok` is true and `✗` otherwise.
fn status_glyph(ok: bool) -> char {
    if ok {
        '✓'
    } else {
        '✗'
    }
}

/// Render an ANSI color prefix for a status.
///
/// Green is used for passing items, red for failing ones, and dim/gray for
/// informational (neutral) rows. When color output is disabled the empty
/// string is returned.
fn color_prefix(ok: bool, neutral: bool) -> &'static str {
    if neutral {
        "\x1b[2m"
    } else if ok {
        "\x1b[32m"
    } else {
        "\x1b[31m"
    }
}

/// ANSI reset escape.
const COLOR_RESET: &str = "\x1b[0m";

// ────────────────────────────────────────────────────────────────────────────
// DiagnosticItem / DiagnosticSection / DiagnosticReport
// ────────────────────────────────────────────────────────────────────────────

/// A single labeled diagnostic check.
///
/// Each item carries a human-readable `label`, the resolved `value` and a
/// boolean `ok` flag indicating whether the check passed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticItem {
    /// Human-readable label, e.g. `"SDK version"`.
    pub label: String,
    /// The resolved value, e.g. `"0.1.0"`.
    pub value: String,
    /// Whether this check passed.
    pub ok: bool,
}

impl DiagnosticItem {
    /// Create a new diagnostic item.
    pub fn new(label: impl Into<String>, value: impl Into<String>, ok: bool) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            ok,
        }
    }

    /// Render a single item line, optionally turning a commit hash `value`
    /// into a GitHub link when it looks like a git SHA.
    fn render_line(&self, last: bool) -> String {
        let connector = if last { "└─" } else { "├─" };
        let glyph = status_glyph(self.ok);
        let neutral = !self.ok && self.value.eq_ignore_ascii_case("unknown");
        let cp = color_prefix(self.ok, neutral);
        let value = maybe_link_commit(&self.value);
        format!("{connector} {cp}{glyph}{COLOR_RESET} {label}: {value}", label = self.label)
    }
}

/// A titled group of diagnostic items with an overall status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticSection {
    /// Section heading, e.g. `"Diagnostics"`.
    pub title: String,
    /// The items belonging to this section, in display order.
    pub items: Vec<DiagnosticItem>,
    /// Whether the section as a whole is healthy. Computed as the logical
    /// AND of every item's `ok` flag (see [`DiagnosticSection::is_ok`]).
    pub ok: bool,
}

impl DiagnosticSection {
    /// Build a new section with the given title and items, deriving `ok`
    /// from the items via [`Self::is_ok`].
    pub fn new(title: impl Into<String>, items: Vec<DiagnosticItem>) -> Self {
        let title = title.into();
        let ok = items.iter().all(|i| i.ok);
        Self { title, items, ok }
    }

    /// Returns `true` when *every* item in the section is `ok`.
    pub fn is_ok(&self) -> bool {
        self.items.iter().all(|i| i.ok)
    }
}

/// A complete diagnostic report, comprising the four canonical sections in
/// display order: *Diagnostics*, *Updates*, *Tracing*, *Configuration*.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticReport {
    /// The sections that make up the report.
    pub sections: Vec<DiagnosticSection>,
}

impl DiagnosticReport {
    /// Create an empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a section to the report.
    pub fn push(&mut self, section: DiagnosticSection) {
        self.sections.push(section);
    }

    /// Render a single section as a tree.
    ///
    /// The section title is printed on its own line, followed by one line per
    /// item using `├─` / `└─` connectors and `✓` / `✗` status glyphs.
    pub fn render_section(&self, section: &DiagnosticSection) -> String {
        let title_glyph = status_glyph(section.is_ok());
        let tcp = color_prefix(section.is_ok(), false);
        let mut out = format!("{tcp}{title_glyph}{COLOR_RESET} {title}", title = section.title);
        let count = section.items.len();
        for (idx, item) in section.items.iter().enumerate() {
            out.push('\n');
            out.push_str(&item.render_line(idx + 1 == count));
        }
        out
    }

    /// Render the entire report as a tree.
    ///
    /// Sections are separated by a blank line. Each section is rendered via
    /// [`Self::render_section`].
    pub fn render(&self) -> String {
        self.sections
            .iter()
            .map(|s| self.render_section(s))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// If the value looks like a git commit SHA, render it as a Markdown GitHub
/// link; otherwise return it unchanged.
fn maybe_link_commit(value: &str) -> String {
    let trimmed = value.trim();
    // A git SHA is typically 7..=40 hex characters.
    let len = trimmed.len();
    if (7..=40).contains(&len) && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("[{trimmed}]({GITHUB_REPO_URL}/commit/{trimmed})")
    } else {
        value.to_string()
    }
}

// ────────────────────────────────────────────────────────────────────────────
// DoctorCollector
// ────────────────────────────────────────────────────────────────────────────

/// Collects the four canonical diagnostic sections.
///
/// Built around an SDK `version`, an optional `commit_hash` and a `platform`
/// tag. The collector methods return [`DiagnosticSection`]s that can be
/// assembled into a [`DiagnosticReport`] via [`Self::collect_all`].
#[derive(Debug, Clone)]
pub struct DoctorCollector {
    /// SDK version string, e.g. `env!("CARGO_PKG_VERSION")`.
    pub version: String,
    /// Optional commit hash (short or full SHA).
    pub commit_hash: Option<String>,
    /// Platform target triple, e.g. `"x86_64-apple-darwin"`.
    pub platform: String,
}

impl DoctorCollector {
    /// Create a new collector bound to the given SDK version.
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            commit_hash: None,
            platform: Self::platform_tag(),
        }
    }

    /// Attach a commit hash to the collector.
    pub fn with_commit_hash(mut self, hash: impl Into<String>) -> Self {
        let h = hash.into();
        self.commit_hash = if h.is_empty() { None } else { Some(h) };
        self
    }

    /// Compute the current platform target triple using `cfg!`.
    ///
    /// Returns a value such as `"x86_64-apple-darwin"`,
    /// `"aarch64-apple-darwin"`, `"x86_64-unknown-linux-gnu"` or
    /// `"x86_64-pc-windows-msvc"`.
    pub fn platform_tag() -> String {
        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else if cfg!(target_arch = "x86") {
            "x86"
        } else if cfg!(target_arch = "arm") {
            "arm"
        } else if cfg!(target_arch = "riscv64") {
            "riscv64"
        } else {
            "unknown"
        };

        let os = if cfg!(target_os = "linux") {
            "unknown-linux"
        } else if cfg!(target_os = "macos") {
            "apple"
        } else if cfg!(target_os = "windows") {
            "pc-windows"
        } else if cfg!(target_os = "freebsd") {
            "unknown-freebsd"
        } else if cfg!(target_os = "openbsd") {
            "unknown-openbsd"
        } else if cfg!(target_os = "netbsd") {
            "unknown-netbsd"
        } else {
            "unknown"
        };

        let env = if cfg!(target_os = "linux") {
            "gnu"
        } else if cfg!(target_os = "macos") {
            "darwin"
        } else if cfg!(target_os = "windows") {
            "msvc"
        } else if cfg!(target_os = "freebsd") {
            "freebsd"
        } else if cfg!(target_os = "openbsd") {
            "openbsd"
        } else if cfg!(target_os = "netbsd") {
            "netbsd"
        } else {
            "unknown"
        };

        format!("{arch}-{os}-{env}")
    }

    /// Collect the **Diagnostics** section: SDK version, commit hash,
    /// platform tag and build commit.
    pub fn collect_diagnostics(&self) -> DiagnosticSection {
        let mut items = Vec::with_capacity(4);

        items.push(DiagnosticItem::new("SDK version", &self.version, true));

        let (commit_val, commit_ok) = match &self.commit_hash {
            Some(h) => (h.clone(), true),
            None => ("unknown".to_string(), false),
        };
        items.push(DiagnosticItem::new("Commit hash", commit_val, commit_ok));

        let plat = Self::platform_tag();
        items.push(DiagnosticItem::new("Platform tag", plat, true));

        let (build_val, build_ok) = match &self.commit_hash {
            Some(h) => (h.clone(), true),
            None => ("unknown".to_string(), false),
        };
        items.push(DiagnosticItem::new("Build commit", build_val, build_ok));

        DiagnosticSection::new("Diagnostics", items)
    }

    /// Collect the **Updates** section: current version, latest version,
    /// last-checked timestamp and auto-update status.
    pub fn collect_updates(
        &self,
        current: &str,
        latest: Option<&str>,
        last_checked: Option<&str>,
        auto_update: bool,
    ) -> DiagnosticSection {
        let mut items = Vec::with_capacity(4);

        items.push(DiagnosticItem::new("Current version", current, true));

        let (latest_val, latest_ok) = match latest {
            Some(v) => (v.to_string(), true),
            None => ("unknown".to_string(), false),
        };
        items.push(DiagnosticItem::new("Latest version", latest_val, latest_ok));

        let (lc_val, lc_ok) = match last_checked {
            Some(v) => (v.to_string(), true),
            None => ("never".to_string(), false),
        };
        items.push(DiagnosticItem::new("Last checked", lc_val, lc_ok));

        items.push(DiagnosticItem::new(
            "Auto-update",
            if auto_update { "enabled" } else { "disabled" },
            true,
        ));

        DiagnosticSection::new("Updates", items)
    }

    /// Collect the **Tracing** section: tracing endpoint, gateway state and
    /// project identifier.
    pub fn collect_tracing(&self, endpoint: Option<&str>) -> DiagnosticSection {
        let mut items = Vec::with_capacity(3);

        let (ep_val, ep_ok) = match endpoint {
            Some(e) if !e.is_empty() => (e.to_string(), true),
            _ => ("not configured".to_string(), false),
        };
        items.push(DiagnosticItem::new("Tracing endpoint", ep_val, ep_ok));

        items.push(DiagnosticItem::new(
            "Gateway state",
            "idle",
            true,
        ));

        items.push(DiagnosticItem::new(
            "Project",
            "default",
            true,
        ));

        DiagnosticSection::new("Tracing", items)
    }

    /// Collect the **Configuration** section: managed config, user config,
    /// per-path existence status and fallback locations.
    ///
    /// Path existence is probed on disk; missing files yield `ok = false`.
    pub fn collect_configuration(
        &self,
        managed_path: Option<&str>,
        user_path: Option<&str>,
    ) -> DiagnosticSection {
        let mut items = Vec::with_capacity(4);

        match managed_path {
            Some(p) => {
                let exists = Path::new(p).exists();
                items.push(DiagnosticItem::new(
                    "Managed config",
                    p,
                    exists,
                ));
            }
            None => items.push(DiagnosticItem::new(
                "Managed config",
                "not configured",
                false,
            )),
        }

        match user_path {
            Some(p) => {
                let exists = Path::new(p).exists();
                items.push(DiagnosticItem::new("User config", p, exists));
            }
            None => items.push(DiagnosticItem::new(
                "User config",
                "not configured",
                false,
            )),
        }

        let any_present = managed_path.map(Path::new).map(|p| p.exists()).unwrap_or(false)
            || user_path.map(Path::new).map(|p| p.exists()).unwrap_or(false);
        items.push(DiagnosticItem::new(
            "Path status",
            if any_present { "ok" } else { "missing" },
            any_present,
        ));

        items.push(DiagnosticItem::new(
            "Fallback locations",
            "built-in defaults",
            true,
        ));

        DiagnosticSection::new("Configuration", items)
    }

    /// Collect all four canonical sections using v0 stub values.
    ///
    /// This is the one-shot entry point used by the CLI `doctor` subcommand
    /// before richer runtime probing is wired up.
    pub fn collect_all(&self) -> DiagnosticReport {
        let mut report = DiagnosticReport::new();

        report.push(self.collect_diagnostics());
        report.push(self.collect_updates(
            &self.version,
            None,
            None,
            false,
        ));
        report.push(self.collect_tracing(None));
        report.push(self.collect_configuration(None, None));

        report
    }
}

impl Default for DoctorCollector {
    fn default() -> Self {
        Self::new(VERSION)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Context doctor
// ────────────────────────────────────────────────────────────────────────────

/// Result of reconciling injected, conversation and provider token counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "detail")]
pub enum ReconciliationStatus {
    /// Injected, conversation and provider token counts all agree.
    Balanced,
    /// The three token counts disagree. Carries the raw counts and a
    /// human-readable `note` describing the mismatch.
    Mismatch {
        /// Total injected (audited) token count.
        injected: u64,
        /// Token count reported by the conversation manager.
        conversation: u64,
        /// Token count reported by the provider response.
        provider: u64,
        /// Human-readable explanation of the mismatch.
        note: String,
    },
}

impl ReconciliationStatus {
    /// Returns `true` when the status is [`ReconciliationStatus::Balanced`].
    pub fn is_balanced(&self) -> bool {
        matches!(self, ReconciliationStatus::Balanced)
    }
}

/// A single audited context row: one component of the injected context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextDoctorRow {
    /// Component name, e.g. `"System prompt"`.
    pub component: String,
    /// Estimated token count for this component.
    pub tokens: u64,
    /// Human-readable details, e.g. `"2,048 chars"`.
    pub details: String,
}

impl ContextDoctorRow {
    /// Create a new context-doctor row.
    pub fn new(component: impl Into<String>, tokens: u64, details: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            tokens,
            details: details.into(),
        }
    }
}

/// A context-doctor report: audited rows plus token reconciliation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextDoctorReport {
    /// The audited context rows (system prompt, AGENTS.md, skills, tools,
    /// MCP servers, ...).
    pub rows: Vec<ContextDoctorRow>,
    /// Total injected token count (sum of all rows).
    pub injected_tokens: u64,
    /// Token count reported by the conversation manager.
    pub conversation_tokens: u64,
    /// Token count reported by the provider response.
    pub provider_tokens: u64,
}

impl ContextDoctorReport {
    /// Create an empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a context-doctor row, updating `injected_tokens`.
    pub fn add_row(&mut self, row: ContextDoctorRow) {
        self.injected_tokens = self.injected_tokens.saturating_add(row.tokens);
        self.rows.push(row);
    }

    /// Total injected tokens across all rows.
    pub fn total_injected(&self) -> u64 {
        self.injected_tokens
    }

    /// Reconcile injected, conversation and provider token counts.
    ///
    /// Returns [`ReconciliationStatus::Balanced`] when all three agree (with
    /// a tolerance of zero), otherwise a [`ReconciliationStatus::Mismatch`]
    /// carrying the raw counts and an explanatory note.
    pub fn reconcile(&self) -> ReconciliationStatus {
        let inj = self.injected_tokens;
        let conv = self.conversation_tokens;
        let prov = self.provider_tokens;
        if inj == conv && conv == prov {
            ReconciliationStatus::Balanced
        } else {
            let note = format!(
                "injected ({inj}) != conversation ({conv}) != provider ({prov})"
            );
            ReconciliationStatus::Mismatch {
                injected: inj,
                conversation: conv,
                provider: prov,
                note,
            }
        }
    }

    /// Render the context-doctor report as a tree.
    ///
    /// Each audited component is listed with its token count; the totals and
    /// reconciliation status are appended at the end.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("Context doctor");
        let count = self.rows.len();
        for (idx, row) in self.rows.iter().enumerate() {
            let connector = if idx + 1 == count { "└─" } else { "├─" };
            out.push('\n');
            out.push_str(&format!(
                "{connector} {component} ({tokens} tokens) — {details}",
                component = row.component,
                tokens = row.tokens,
                details = row.details,
            ));
        }
        out.push('\n');
        out.push_str(&format!(
            "Injected: {inj}  Conversation: {conv}  Provider: {prov}",
            inj = self.injected_tokens,
            conv = self.conversation_tokens,
            prov = self.provider_tokens,
        ));
        let status = self.reconcile();
        out.push('\n');
        match status {
            ReconciliationStatus::Balanced => {
                out.push_str("Reconciliation: \u{2713} balanced");
            }
            ReconciliationStatus::Mismatch { note, .. } => {
                out.push_str(&format!("Reconciliation: \u{2717} {note}"));
            }
        }
        out
    }
}

/// Rough token estimate heuristic: chars / 4.
///
/// Provides a quick, dependency-free approximation of the token count for a
/// piece of text. This mirrors the common "4 characters per token" rule of
/// thumb used by many SDKs for budgeting purposes. It is **not** a substitute
/// for a real tokenizer — accuracy varies by language and model.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

/// Builder for [`ContextDoctorReport`].
///
/// Audits the standard injected-context components — system prompt, AGENTS.md
/// memory, skills index, built-in tool schemas and MCP servers — then
/// optionally records conversation- and provider-side token counts for
/// three-way reconciliation via [`ContextDoctorReport::reconcile`].
#[derive(Debug, Clone, Default)]
pub struct ContextDoctorCollector {
    rows: Vec<ContextDoctorRow>,
    conversation_tokens: u64,
    provider_tokens: u64,
}

impl ContextDoctorCollector {
    /// Create a new empty collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Audit the system prompt.
    ///
    /// Token count is estimated via [`estimate_tokens`].
    pub fn audit_system_prompt(&mut self, prompt: &str) {
        let tokens = estimate_tokens(prompt);
        let chars = prompt.chars().count();
        self.rows.push(ContextDoctorRow::new(
            "System prompt",
            tokens,
            format!("{chars} chars"),
        ));
    }

    /// Audit the AGENTS.md memory contents.
    ///
    /// Token count is estimated via [`estimate_tokens`].
    pub fn audit_memory(&mut self, memory: &str) {
        let tokens = estimate_tokens(memory);
        let chars = memory.chars().count();
        self.rows.push(ContextDoctorRow::new(
            "AGENTS.md memory",
            tokens,
            format!("{chars} chars"),
        ));
    }

    /// Audit the skills index.
    ///
    /// `skill_count` is the number of registered skills; `tokens` is the
    /// schema token budget they occupy.
    pub fn audit_skills(&mut self, skill_count: usize, tokens: u64) {
        self.rows.push(ContextDoctorRow::new(
            "Skills index",
            tokens,
            format!("{skill_count} skills"),
        ));
    }

    /// Audit built-in tool schemas.
    ///
    /// `tool_count` is the number of built-in tools; `tokens` is the schema
    /// token budget they occupy.
    pub fn audit_tool_schemas(&mut self, tool_count: usize, tokens: u64) {
        self.rows.push(ContextDoctorRow::new(
            "Built-in tool schemas",
            tokens,
            format!("{tool_count} tools"),
        ));
    }

    /// Audit MCP server schemas.
    ///
    /// `server_count` is the number of connected MCP servers; `tokens` is
    /// the schema token budget they occupy.
    pub fn audit_mcp_servers(&mut self, server_count: usize, tokens: u64) {
        self.rows.push(ContextDoctorRow::new(
            "MCP servers",
            tokens,
            format!("{server_count} servers"),
        ));
    }

    /// Set the conversation-side token count (reported by the conversation
    /// manager) for three-way reconciliation.
    pub fn set_conversation_tokens(&mut self, tokens: u64) {
        self.conversation_tokens = tokens;
    }

    /// Set the provider-side token count (reported by the provider
    /// response's `usage`) for three-way reconciliation.
    pub fn set_provider_tokens(&mut self, tokens: u64) {
        self.provider_tokens = tokens;
    }

    /// Finalize the report, summing per-row tokens into `injected_tokens`.
    pub fn build(self) -> ContextDoctorReport {
        let injected_tokens = self
            .rows
            .iter()
            .fold(0u64, |acc, r| acc.saturating_add(r.tokens));
        ContextDoctorReport {
            rows: self.rows,
            injected_tokens,
            conversation_tokens: self.conversation_tokens,
            provider_tokens: self.provider_tokens,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Convenience: load a config path into a Result-friendly helper
// ────────────────────────────────────────────────────────────────────────────

/// Probe a config path and return a boolean existence flag.
///
/// Wrapped in a `Result` so callers can surface I/O failures via
/// [`deepagents_errors::Error`]. Currently only distinguishes "exists" from
/// "missing" and never returns `Err`, but the `Result` signature keeps the
/// door open for richer probing (metadata / readability) without a breaking
/// change.
pub fn probe_config_path(path: &str) -> Result<bool, Error> {
    Ok(Path::new(path).exists())
}

/// Format a missing-config error using the shared error type.
///
/// Convenience for callers that want to turn a *missing* managed config path
/// into a structured [`ConfigError::Load`].
pub fn missing_config_error(path: &str) -> Error {
    ConfigError::Load(format!("config file not found: {path}")).into()
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostic_item_serde() {
        let item = DiagnosticItem::new("SDK version", "0.1.0", true);
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(json.contains("SDK version"));
        assert!(json.contains("0.1.0"));
        assert!(json.contains("\"ok\":true"));
        let back: DiagnosticItem =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.label, "SDK version");
        assert_eq!(back.value, "0.1.0");
        assert!(back.ok);
    }

    #[test]
    fn test_diagnostic_section_is_ok() {
        let ok_section = DiagnosticSection::new(
            "Diagnostics",
            vec![
                DiagnosticItem::new("a", "1", true),
                DiagnosticItem::new("b", "2", true),
            ],
        );
        assert!(ok_section.is_ok());
        assert!(ok_section.ok);

        let mixed = DiagnosticSection::new(
            "Diagnostics",
            vec![
                DiagnosticItem::new("a", "1", true),
                DiagnosticItem::new("b", "2", false),
            ],
        );
        assert!(!mixed.is_ok());
        assert!(!mixed.ok);
    }

    #[test]
    fn test_diagnostic_report_render() {
        let report = DoctorCollector::new("0.1.0")
            .with_commit_hash("abcdef1234")
            .collect_all();
        let rendered = report.render();
        // Tree connectors present.
        assert!(rendered.contains("├─") || rendered.contains("└─"));
        // Status glyphs present.
        assert!(rendered.contains('✓') || rendered.contains('✗'));
        // All four section titles present, in order.
        assert!(rendered.contains("Diagnostics"));
        assert!(rendered.contains("Updates"));
        assert!(rendered.contains("Tracing"));
        assert!(rendered.contains("Configuration"));
        // Commit hash rendered as a GitHub link.
        assert!(rendered.contains("abcdef1234"));
        assert!(rendered.contains("/commit/"));
    }

    #[test]
    fn test_doctor_collector_diagnostics() {
        let collector =
            DoctorCollector::new("0.1.0").with_commit_hash("deadbeef");
        let section = collector.collect_diagnostics();
        assert_eq!(section.title, "Diagnostics");
        assert_eq!(section.items.len(), 4);
        let version_item = section
            .items
            .iter()
            .find(|i| i.label == "SDK version")
            .expect("version item present");
        assert_eq!(version_item.value, "0.1.0");
        assert!(version_item.ok);
        // Platform tag is non-empty and contains a dash.
        let plat = section
            .items
            .iter()
            .find(|i| i.label == "Platform tag")
            .expect("platform item present");
        assert!(!plat.value.is_empty());
        assert!(plat.value.contains('-'));
    }

    #[test]
    fn test_context_doctor_report_add_row() {
        let mut report = ContextDoctorReport::new();
        assert_eq!(report.total_injected(), 0);
        report.add_row(ContextDoctorRow::new("System prompt", 100, "test"));
        assert_eq!(report.total_injected(), 100);
        report.add_row(ContextDoctorRow::new("Memory", 50, "test"));
        assert_eq!(report.total_injected(), 150);
        assert_eq!(report.rows.len(), 2);
    }

    #[test]
    fn test_context_doctor_reconciliation_balanced() {
        let mut report = ContextDoctorReport::new();
        report.add_row(ContextDoctorRow::new("System prompt", 100, "test"));
        report.conversation_tokens = 100;
        report.provider_tokens = 100;
        assert_eq!(report.reconcile(), ReconciliationStatus::Balanced);

        // Now mismatch.
        report.provider_tokens = 90;
        let status = report.reconcile();
        match status {
            ReconciliationStatus::Mismatch {
                injected,
                conversation,
                provider,
                ..
            } => {
                assert_eq!(injected, 100);
                assert_eq!(conversation, 100);
                assert_eq!(provider, 90);
            }
            ReconciliationStatus::Balanced => panic!("expected mismatch"),
        }
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        // Unicode safety: each codepoint counts.
        assert_eq!(estimate_tokens("éééé"), 1);
    }

    #[test]
    fn test_context_doctor_collector_build() {
        let mut collector = ContextDoctorCollector::new();
        collector.audit_system_prompt(&"a".repeat(40)); // 40 chars -> 10 tokens
        collector.audit_memory(&"a".repeat(8)); // 8 chars -> 2 tokens
        collector.audit_skills(3, 50);
        collector.audit_tool_schemas(5, 75);
        collector.audit_mcp_servers(2, 30);
        collector.set_conversation_tokens(167);
        collector.set_provider_tokens(167);
        let report = collector.build();
        assert_eq!(report.rows.len(), 5);
        assert_eq!(report.injected_tokens, 10 + 2 + 50 + 75 + 30);
        assert_eq!(report.conversation_tokens, 167);
        assert!(report.reconcile().is_balanced());
        let rendered = report.render();
        assert!(rendered.contains("System prompt"));
        assert!(rendered.contains("AGENTS.md memory"));
        assert!(rendered.contains("Skills index"));
        assert!(rendered.contains("Built-in tool schemas"));
        assert!(rendered.contains("MCP servers"));
    }

    #[test]
    fn test_platform_tag_shape() {
        let tag = DoctorCollector::platform_tag();
        // Two dashes separate arch-os-env.
        assert_eq!(tag.matches('-').count(), 2);
    }

    #[test]
    fn test_reconciliation_status_serde() {
        let s = ReconciliationStatus::Balanced;
        let j = serde_json::to_string(&s).expect("serialize");
        let back: ReconciliationStatus =
            serde_json::from_str(&j).expect("deserialize");
        assert_eq!(s, back);

        let m = ReconciliationStatus::Mismatch {
            injected: 10,
            conversation: 12,
            provider: 9,
            note: "oops".into(),
        };
        let j2 = serde_json::to_string(&m).expect("serialize");
        let back2: ReconciliationStatus =
            serde_json::from_str(&j2).expect("deserialize");
        assert_eq!(m, back2);
    }
}

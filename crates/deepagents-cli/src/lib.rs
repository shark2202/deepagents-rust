//! clap, 13 subcommands (Q13)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.
//!
//! This crate defines the `deepagents` command-line interface using clap's
//! derive API. It models the thirteen user-facing entry points (the default
//! TUI mode, single-prompt mode, print mode, and ten explicit subcommands)
//! and dispatches them through [`CliRunner`]. The runner is intentionally
//! minimal in v0: most handlers are stubs that emit a "not yet implemented"
//! notice to stderr and return [`CliResult::Success`], so the rest of the
//! workspace can wire real handlers incrementally without touching the CLI
//! surface.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

use clap::{Parser, Subcommand};
use deepagents_errors::Error;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── Top-level CLI ─────────────────────────────────────────────────────────

/// The top-level `deepagents` CLI.
///
/// When invoked with no subcommand the binary enters its default TUI mode.
/// The `-p` / `--prompt` flag selects single-prompt mode, `--print` selects
/// print mode, and [`Commands`] selects one of the ten explicit subcommands.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "deepagents",
    bin_name = "deepagents",
    version = VERSION,
    about = "LangChain Deep Agents SDK — Rust port built on rig",
    long_about = "LangChain Deep Agents SDK (Rust port, built on rig).\n\n\
                  Run with no arguments to start the default TUI mode, pass \
                  -p/--prompt for single-prompt mode, --print for print mode, \
                  or select one of the explicit subcommands."
)]
pub struct Cli {
    /// Single-prompt mode: run one turn against the model and exit.
    #[arg(short = 'p', long = "prompt")]
    pub prompt: Option<String>,

    /// Print mode: emit the model's final answer to stdout and exit.
    #[arg(long = "print")]
    pub print: bool,

    /// Override the configured model for this invocation.
    #[arg(long = "model")]
    pub model: Option<String>,

    /// Enable debug / verbose diagnostics for this invocation.
    #[arg(long = "debug")]
    pub debug: bool,

    /// Override the agent's display name for this invocation.
    #[arg(long = "name")]
    pub name: Option<String>,

    /// Optional subcommand. When `None`, the binary enters default TUI mode.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

// ─── Subcommands ──────────────────────────────────────────────────────────

/// The ten explicit subcommands exposed by the `deepagents` CLI.
///
/// Together with the default (no-subcommand) TUI mode, single-prompt mode
/// (`-p`), and print mode (`--print`), these make up the thirteen user-facing
/// entry points described in SPEC Q13.
#[derive(Debug, Clone, Subcommand)]
pub enum Commands {
    /// `deepagents serve` — start the HTTP/ACP server.
    Serve(ServeArgs),
    /// `deepagents resume` — resume a previous session.
    Resume(ResumeArgs),
    /// `deepagents config` — inspect / edit configuration.
    Config(ConfigArgs),
    /// `deepagents doctor` — run environment diagnostics.
    Doctor(DoctorArgs),
    /// `deepagents context-doctor` — run context-window diagnostics.
    ContextDoctor(ContextDoctorArgs),
    /// `deepagents mcp` — manage MCP servers.
    Mcp(McpArgs),
    /// `deepagents plugins` — manage plugins.
    Plugins(PluginsArgs),
    /// `deepagents skills` — manage skills.
    Skills(SkillsArgs),
    /// `deepagents hooks` — manage / run hooks.
    Hooks(HooksArgs),
    /// `deepagents update` — self-update / version check.
    Update(UpdateArgs),
}

/// Arguments for `deepagents serve`.
#[derive(Debug, Clone, Parser)]
pub struct ServeArgs {
    /// Port to bind the server to.
    #[arg(long)]
    pub port: Option<u16>,
}

/// Arguments for `deepagents resume`.
#[derive(Debug, Clone, Parser)]
pub struct ResumeArgs {
    /// Session id to resume. If omitted, a picker is shown.
    #[arg(long = "session-id")]
    pub session_id: Option<String>,
}

/// Arguments for `deepagents config`.
#[derive(Debug, Clone, Parser)]
pub struct ConfigArgs {
    /// List all configuration values.
    #[arg(long)]
    pub list: bool,
    /// Get a single configuration value by key.
    #[arg(long)]
    pub get: Option<String>,
    /// Set a single configuration value (`key=value` or `key value`).
    #[arg(long)]
    pub set: Option<String>,
}

/// Arguments for `deepagents doctor`.
#[derive(Debug, Clone, Parser)]
pub struct DoctorArgs {}

/// Arguments for `deepagents context-doctor`.
#[derive(Debug, Clone, Parser)]
pub struct ContextDoctorArgs {}

/// Arguments for `deepagents mcp`.
#[derive(Debug, Clone, Parser)]
pub struct McpArgs {
    /// List configured MCP servers.
    #[arg(long)]
    pub list: bool,
    /// Add an MCP server (e.g. `name url`).
    #[arg(long)]
    pub add: Option<String>,
    /// Remove an MCP server by name.
    #[arg(long)]
    pub remove: Option<String>,
}

/// Arguments for `deepagents plugins`.
#[derive(Debug, Clone, Parser)]
pub struct PluginsArgs {
    /// List installed plugins.
    #[arg(long)]
    pub list: bool,
    /// Install a plugin by name or path.
    #[arg(long)]
    pub install: Option<String>,
    /// Remove an installed plugin by name.
    #[arg(long)]
    pub remove: Option<String>,
}

/// Arguments for `deepagents skills`.
#[derive(Debug, Clone, Parser)]
pub struct SkillsArgs {
    /// List configured skills.
    #[arg(long)]
    pub list: bool,
    /// Add a skill by path or identifier.
    #[arg(long)]
    pub add: Option<String>,
}

/// Arguments for `deepagents hooks`.
#[derive(Debug, Clone, Parser)]
pub struct HooksArgs {
    /// List configured hooks.
    #[arg(long)]
    pub list: bool,
    /// Run a named hook immediately.
    #[arg(long)]
    pub run: Option<String>,
}

/// Arguments for `deepagents update`.
#[derive(Debug, Clone, Parser)]
pub struct UpdateArgs {
    /// Check for an available update without applying it.
    #[arg(long)]
    pub check: bool,
    /// Force the update even when already on the latest version.
    #[arg(long)]
    pub force: bool,
}

// ─── Result ───────────────────────────────────────────────────────────────

/// The coarse-grained outcome of a CLI invocation.
///
/// `run` returns this rather than `Result<(), Error>` so that handlers can
/// distinguish between clean success, a structured exit code, and a soft
/// error message that the binary entry point can render before exiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliResult {
    /// The command completed successfully.
    Success,
    /// The command completed but requests a specific process exit code.
    Exit(i32),
    /// The command failed with a human-readable message (no panic).
    Error(String),
}

// ─── Runner ───────────────────────────────────────────────────────────────

/// A parsed and dispatchable CLI invocation.
///
/// [`CliRunner::parse`] mirrors `Cli::parse` but keeps the runner as the
/// single public entry point so the rest of the workspace does not need to
/// import clap directly. Use [`CliRunner::parse_from`] in tests.
#[derive(Debug, Clone)]
pub struct CliRunner {
    cli: Cli,
}

impl CliRunner {
    /// Construct a runner wrapping an already-parsed [`Cli`].
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            cli: Cli {
                prompt: None,
                print: false,
                model: None,
                debug: false,
                name: None,
                command: None,
            },
        }
    }

    /// Parse the runner from `std::env::args`.
    ///
    /// Panics if argument parsing fails (matching clap's default behavior);
    /// the binary entry point is expected to own the process lifecycle.
    pub fn parse() -> Self {
        Self {
            cli: Cli::parse(),
        }
    }

    /// Parse the runner from an explicit iterator of arguments.
    ///
    /// Intended for integration tests where the ambient process args are not
    /// under the test's control.
    pub fn parse_from<I, S>(iter: I) -> Cli
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        // Normalize every item to an owned `String` (which clap accepts as
        // `Into<OsString> + Clone`) so callers can pass `&str`, `String`, or
        // any other `Into<String>` source uniformly.
        let args: Vec<String> = iter.into_iter().map(|s| s.into()).collect();
        Cli::parse_from(args)
    }

    /// The parsed prompt flag (`-p` / `--prompt`), if any.
    pub fn prompt(&self) -> Option<&str> {
        self.cli.prompt.as_deref()
    }

    /// Whether `--print` was supplied.
    pub fn print(&self) -> bool {
        self.cli.print
    }

    /// The parsed `--model` override, if any.
    pub fn model(&self) -> Option<&str> {
        self.cli.model.as_deref()
    }

    /// Whether `--debug` was supplied.
    pub fn debug(&self) -> bool {
        self.cli.debug
    }

    /// The parsed `--name` override, if any.
    pub fn name(&self) -> Option<&str> {
        self.cli.name.as_deref()
    }

    /// The selected subcommand, if any.
    pub fn command(&self) -> Option<&Commands> {
        self.cli.command.as_ref()
    }

    /// Dispatch the parsed invocation to the appropriate handler.
    ///
    /// In v0 most handlers are stubs: they emit a "not yet implemented"
    /// notice to stderr and return [`CliResult::Success`]. Real handlers
    /// are wired in by their owning crates without changing this surface.
    pub async fn run(&self) -> Result<CliResult, Error> {
        match &self.cli.command {
            None => {
                if self.cli.print {
                    self.not_yet_implemented("print mode")
                } else if self.cli.prompt.is_some() {
                    self.not_yet_implemented("single-prompt mode")
                } else {
                    self.not_yet_implemented("default TUI mode")
                }
            }
            Some(Commands::Serve(_)) => self.not_yet_implemented("serve"),
            Some(Commands::Resume(_)) => self.not_yet_implemented("resume"),
            Some(Commands::Config(_)) => self.not_yet_implemented("config"),
            Some(Commands::Doctor(_)) => self.not_yet_implemented("doctor"),
            Some(Commands::ContextDoctor(_)) => self.not_yet_implemented("context-doctor"),
            Some(Commands::Mcp(_)) => self.not_yet_implemented("mcp"),
            Some(Commands::Plugins(_)) => self.not_yet_implemented("plugins"),
            Some(Commands::Skills(_)) => self.not_yet_implemented("skills"),
            Some(Commands::Hooks(_)) => self.not_yet_implemented("hooks"),
            Some(Commands::Update(_)) => self.not_yet_implemented("update"),
        }
    }

    /// Emit a "not yet implemented" notice for `what` and return success.
    fn not_yet_implemented(&self, what: &str) -> Result<CliResult, Error> {
        eprintln!("deepagents: {what}: not yet implemented");
        Ok(CliResult::Success)
    }
}

impl Default for CliRunner {
    fn default() -> Self {
        // `Cli::default()` would also work, but clap's derive `Parser` is
        // guaranteed to derive `Default` only when every field is `Default`,
        // which holds here. Construct explicitly to stay forward-compatible.
        Self::new()
    }
}

impl fmt::Display for CliResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliResult::Success => f.write_str("success"),
            CliResult::Exit(code) => write!(f, "exit({code})"),
            CliResult::Error(msg) => write!(f, "error: {msg}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::needless_pass_by_value)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_default() {
        let cli = CliRunner::parse_from(["deepagents"]);
        assert!(cli.prompt.is_none());
        assert!(!cli.print);
        assert!(cli.model.is_none());
        assert!(cli.name.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_parse_prompt() {
        let cli = CliRunner::parse_from(["deepagents", "-p", "hello"]);
        assert_eq!(cli.prompt.as_deref(), Some("hello"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_parse_prompt_long() {
        let cli = CliRunner::parse_from(["deepagents", "--prompt", "hi there"]);
        assert_eq!(cli.prompt.as_deref(), Some("hi there"));
    }

    #[test]
    fn test_parse_print() {
        let cli = CliRunner::parse_from(["deepagents", "--print"]);
        assert!(cli.print);
    }

    #[test]
    fn test_parse_model_and_name() {
        let cli = CliRunner::parse_from([
            "deepagents",
            "--model",
            "gpt-4o",
            "--name",
            "alice",
            "--debug",
        ]);
        assert_eq!(cli.model.as_deref(), Some("gpt-4o"));
        assert_eq!(cli.name.as_deref(), Some("alice"));
        assert!(cli.debug);
    }

    #[test]
    fn test_parse_serve() {
        let cli = CliRunner::parse_from(["deepagents", "serve", "--port", "8080"]);
        match cli.command {
            Some(Commands::Serve(args)) => assert_eq!(args.port, Some(8080)),
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_resume() {
        let cli = CliRunner::parse_from([
            "deepagents",
            "resume",
            "--session-id",
            "abc-123",
        ]);
        match cli.command {
            Some(Commands::Resume(args)) => assert_eq!(args.session_id.as_deref(), Some("abc-123")),
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_doctor() {
        let cli = CliRunner::parse_from(["deepagents", "doctor"]);
        assert!(matches!(cli.command, Some(Commands::Doctor(_))));
    }

    #[test]
    fn test_parse_context_doctor() {
        let cli = CliRunner::parse_from(["deepagents", "context-doctor"]);
        assert!(matches!(cli.command, Some(Commands::ContextDoctor(_))));
    }

    #[test]
    fn test_parse_config() {
        let cli = CliRunner::parse_from(["deepagents", "config", "--get", "model"]);
        match cli.command {
            Some(Commands::Config(args)) => {
                assert_eq!(args.get.as_deref(), Some("model"));
                assert!(!args.list);
                assert!(args.set.is_none());
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_config_list() {
        let cli = CliRunner::parse_from(["deepagents", "config", "--list"]);
        match cli.command {
            Some(Commands::Config(args)) => assert!(args.list),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_mcp() {
        let cli = CliRunner::parse_from(["deepagents", "mcp", "--add", "time http://x"]);
        match cli.command {
            Some(Commands::Mcp(args)) => assert_eq!(args.add.as_deref(), Some("time http://x")),
            other => panic!("expected Mcp, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_plugins() {
        let cli = CliRunner::parse_from(["deepagents", "plugins", "--install", "foo"]);
        match cli.command {
            Some(Commands::Plugins(args)) => assert_eq!(args.install.as_deref(), Some("foo")),
            other => panic!("expected Plugins, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_skills() {
        let cli = CliRunner::parse_from(["deepagents", "skills", "--add", "./skill"]);
        match cli.command {
            Some(Commands::Skills(args)) => assert_eq!(args.add.as_deref(), Some("./skill")),
            other => panic!("expected Skills, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_hooks() {
        let cli = CliRunner::parse_from(["deepagents", "hooks", "--run", "pre-commit"]);
        match cli.command {
            Some(Commands::Hooks(args)) => assert_eq!(args.run.as_deref(), Some("pre-commit")),
            other => panic!("expected Hooks, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_update() {
        let cli = CliRunner::parse_from(["deepagents", "update", "--check"]);
        match cli.command {
            Some(Commands::Update(args)) => {
                assert!(args.check);
                assert!(!args.force);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_update_force() {
        let cli = CliRunner::parse_from(["deepagents", "update", "--force"]);
        match cli.command {
            Some(Commands::Update(args)) => {
                assert!(!args.check);
                assert!(args.force);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn test_cli_result_traits() {
        assert_eq!(CliResult::Success, CliResult::Success.clone());
        assert_eq!(CliResult::Exit(2), CliResult::Exit(2));
        assert_eq!(
            CliResult::Error("boom".into()),
            CliResult::Error("boom".into())
        );
        assert_ne!(CliResult::Success, CliResult::Exit(0));
        assert_eq!(format!("{}", CliResult::Success), "success");
        assert_eq!(format!("{}", CliResult::Exit(7)), "exit(7)");
        assert_eq!(format!("{}", CliResult::Error("x".into())), "error: x");
    }

    #[tokio::test]
    async fn test_run_default_is_stub() {
        let runner = CliRunner::new();
        let res = runner.run().await.unwrap();
        assert_eq!(res, CliResult::Success);
    }

    #[tokio::test]
    async fn test_run_doctor_is_stub() {
        let cli = CliRunner::parse_from(["deepagents", "doctor"]);
        let runner = CliRunner { cli };
        let res = runner.run().await.unwrap();
        assert_eq!(res, CliResult::Success);
    }

    #[test]
    fn test_version_nonempty() {
        assert!(!VERSION.is_empty());
    }
}

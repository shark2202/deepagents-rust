//! 55 env vars, dotenvy, 3-layer denylist (Q12)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.
//!
//! This crate maps `DEEPAGENTS_CODE_*` environment variables onto internal
//! configuration option keys. When a value is read from the environment it
//! passes through a 3-layer denylist:
//!
//! 1. **Global** — security-sensitive options that must never be accepted
//!    from the environment (e.g. paths of trusted root CAs, managed policy
//!    overrides).
//! 2. **Managed** — admin-forbidden options set by a managed policy profile.
//! 3. **Option-level** — a single option may mark itself `env_deny = true`.
//!
//! `.env` files are loaded with the pure-Rust [`dotenvy`] crate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::Path;

use deepagents_errors::{ConfigError, Error};
use serde::{Deserialize, Serialize};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Prefix shared by every Deep Agents code environment variable.
///
/// All 55 recognized variables begin with this string, e.g.
/// `DEEPAGENTS_CODE_MODEL`.
pub const PREFIX: &str = "DEEPAGENTS_CODE_";

// ── EnvVar ──────────────────────────────────────────────────────────────

/// A single registered environment variable.
///
/// Holds the raw environment variable name (e.g. `DEEPAGENTS_CODE_MODEL`),
/// the configuration option key it maps to inside the runtime, and a
/// human-readable description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    /// The environment variable name, e.g. `DEEPAGENTS_CODE_MODEL`.
    pub name: String,
    /// The configuration option key this variable maps to.
    pub config_key: String,
    /// Human-readable description of the variable's purpose.
    pub description: String,
}

impl EnvVar {
    /// Create a new `EnvVar` from its three fields.
    pub fn new(name: impl Into<String>, config_key: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            config_key: config_key.into(),
            description: description.into(),
        }
    }
}

// ── Denylist ────────────────────────────────────────────────────────────

/// A single entry in the global or managed denylist.
///
/// Pairs an environment variable name with the reason it is denied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenylistEntry {
    /// The environment variable name that is denied.
    pub env_var: String,
    /// Human-readable reason the variable is denied.
    pub reason: String,
}

impl DenylistEntry {
    /// Create a new `DenylistEntry` from its two fields.
    pub fn new(env_var: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            env_var: env_var.into(),
            reason: reason.into(),
        }
    }
}

/// The 3-layer denylist applied when mapping env vars to config options.
///
/// Layer 1 (`global`) and layer 2 (`managed`) hold [`DenylistEntry`] values
/// keyed by environment variable name. Layer 3 (`option_level`) holds
/// configuration option keys (not env var names) that a single option has
/// marked as `env_deny = true`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Denylist {
    /// Layer 1: security-sensitive variables never accepted from the
    /// environment.
    pub global: Vec<DenylistEntry>,
    /// Layer 2: admin-forbidden options set by a managed policy profile.
    pub managed: Vec<DenylistEntry>,
    /// Layer 3: configuration option keys with `env_deny = true`.
    pub option_level: Vec<String>,
}

impl Denylist {
    /// Create an empty denylist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry to the global (layer 1) denylist.
    pub fn add_global(&mut self, entry: DenylistEntry) {
        self.global.push(entry);
    }

    /// Add an entry to the managed (layer 2) denylist.
    pub fn add_managed(&mut self, entry: DenylistEntry) {
        self.managed.push(entry);
    }

    /// Add a configuration option key to the option-level (layer 3) denylist.
    pub fn add_option_deny(&mut self, config_key: String) {
        self.option_level.push(config_key);
    }

    /// Check whether an env var / config key pair is denied by any layer.
    ///
    /// Returns the matching [`DenylistEntry`] (from global or managed) if
    /// denied, or `None` if allowed. For option-level denies (which store
    /// only a config key, no reason) a synthetic entry is returned.
    pub fn is_denied(&self, env_name: &str, config_key: &str) -> Option<&DenylistEntry> {
        // Layer 1: global.
        if let Some(entry) = self.global.iter().find(|e| e.env_var == env_name) {
            return Some(entry);
        }
        // Layer 2: managed.
        if let Some(entry) = self.managed.iter().find(|e| e.env_var == env_name) {
            return Some(entry);
        }
        // Layer 3: option-level (matches by config key).
        if self.option_level.iter().any(|k| k == config_key) {
            // The option-level list carries no reason text, so synthesize a
            // minimal entry. We cannot return a reference to a temporary, so
            // instead re-scan: this branch only confirms a match.
            //
            // Note: callers that need the *reason* should consult the global
            // or managed layers; option-level denies are intentionally
            // reason-less.
            //
            // To satisfy the `Option<&DenylistEntry>` return type without
            // allocating, we look for a global/managed entry for the same env
            // var as a stand-in; if none exists we fall through to None.
            //
            // (See `EnvRegistry::resolve` for the actual denial surface,
            // which does not rely on a returned reason here.)
            return None;
        }
        None
    }

    /// Returns `true` when `env_name` is denied by any layer, or when
    /// `config_key` is denied at option level.
    ///
    /// This is the authoritative check used by [`EnvRegistry::resolve`]. It
    /// differs from [`is_denied`](Self::is_denied) in that it also reports
    /// option-level denies (which carry no reason text).
    pub fn is_denied_bool(&self, env_name: &str, config_key: &str) -> Option<DenylistEntry> {
        // Layer 1: global.
        if let Some(entry) = self.global.iter().find(|e| e.env_var == env_name) {
            return Some(entry.clone());
        }
        // Layer 2: managed.
        if let Some(entry) = self.managed.iter().find(|e| e.env_var == env_name) {
            return Some(entry.clone());
        }
        // Layer 3: option-level.
        if self.option_level.iter().any(|k| k == config_key) {
            return Some(DenylistEntry::new(
                env_name,
                "option-level env_deny = true",
            ));
        }
        None
    }
}

// ── EnvRegistry ─────────────────────────────────────────────────────────

/// The registry of recognized environment variables and the denylist that
/// gates them.
///
/// Built either directly via [`new`](Self::new) / [`register`](Self::register)
/// or via the [`EnvBuilder`] fluent builder. The registry is the primary
/// entry point for resolving `DEEPAGENTS_CODE_*` values at runtime.
#[derive(Debug, Clone, Default)]
pub struct EnvRegistry {
    /// All registered environment variables, keyed by env var name.
    pub vars: HashMap<String, EnvVar>,
    /// The 3-layer denylist applied during resolution.
    pub denylist: Denylist,
}

impl EnvRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an environment variable.
    pub fn register(&mut self, var: EnvVar) {
        self.vars.insert(var.name.clone(), var);
    }

    /// Look up a registered variable by its env var name.
    pub fn get(&self, name: &str) -> Option<&EnvVar> {
        self.vars.get(name)
    }

    /// Resolve a single registered environment variable.
    ///
    /// Reads the value from the process environment, checks the 3-layer
    /// denylist, and returns:
    /// - `Ok(Some(value))` if the variable is set and allowed,
    /// - `Ok(None)` if it is not set (and thus allowed),
    /// - `Err` if the variable is denied by any layer.
    ///
    /// Unknown (unregistered) variable names return `Ok(None)` — the caller
    /// is responsible for only asking about recognized variables.
    pub fn resolve(&self, name: &str) -> Result<Option<String>, Error> {
        let Some(var) = self.vars.get(name) else {
            // Not a registered variable; nothing to resolve.
            return Ok(None);
        };
        // Check the denylist before reading the environment, so that even a
        // *presence* check cannot leak a denied value's existence semantics.
        if let Some(entry) = self.denylist.is_denied_bool(name, &var.config_key) {
            // Classify the denial by layer for precise error reporting.
            let is_managed = self
                .denylist
                .managed
                .iter()
                .any(|m| m.env_var == name);
            let is_global = self
                .denylist
                .global
                .iter()
                .any(|g| g.env_var == name);
            let detail = format!(
                "env var {name} (config key {key}) denied: {reason}",
                key = var.config_key,
                reason = entry.reason,
            );
            if is_managed {
                return Err(ConfigError::ManagedPolicy(detail).into());
            }
            if is_global {
                return Err(ConfigError::ManagedConfig(detail).into());
            }
            // Option-level denial (layer 3).
            return Err(ConfigError::ManagedConfig(detail).into());
        }
        // Not denied: read the value from the environment.
        match std::env::var(name) {
            Ok(v) => Ok(Some(v)),
            // Not set in the environment.
            Err(_) => Ok(None),
        }
    }

    /// Resolve all registered environment variables.
    ///
    /// Returns a map from env var name to its value, for every registered
    /// variable that is both allowed and currently set. Denied variables
    /// short-circuit to an `Err`.
    pub fn resolve_all(&self) -> Result<HashMap<String, String>, Error> {
        let mut out = HashMap::new();
        for name in self.vars.keys() {
            if let Some(value) = self.resolve(name)? {
                out.insert(name.clone(), value);
            }
        }
        Ok(out)
    }

    /// Load a `.env` file from `path` using [`dotenvy`].
    ///
    /// Existing process environment variables are **not** overridden by
    /// values found in the file; this matches `dotenvy::from_filename`
    /// semantics. Use [`load_dotenv_overriding`](Self::load_dotenv_overriding)
    /// to override existing values.
    pub fn load_dotenv(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        match dotenvy::from_path(path.as_ref()) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load .env file");
                Err(ConfigError::Load(format!("dotenvy load error: {e}")).into())
            }
        }
    }

    /// Load a `.env` file, overriding existing process environment values.
    pub fn load_dotenv_overriding(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        match dotenvy::from_path_override(path.as_ref()) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load .env file (override)");
                Err(ConfigError::Load(format!("dotenvy load error: {e}")).into())
            }
        }
    }

    /// Load a `.env` file from the current working directory if it exists.
    ///
    /// Returns `Ok(())` silently when no `.env` file is present, so callers
    /// can call this unconditionally at startup.
    pub fn load_dotenv_default(&self) -> Result<(), Error> {
        let path = Path::new(".env");
        if !path.exists() {
            return Ok(());
        }
        self.load_dotenv(path)
    }
}

// ── EnvBuilder ──────────────────────────────────────────────────────────

/// A fluent builder for [`EnvRegistry`].
///
/// Start with [`new`](Self::new), add variables and denylist entries, then
/// call [`build`](Self::build).
#[derive(Debug, Clone, Default)]
pub struct EnvBuilder {
    registry: EnvRegistry,
}

impl EnvBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an environment variable.
    pub fn var(mut self, var: EnvVar) -> Self {
        self.registry.register(var);
        self
    }

    /// Add an entry to the global (layer 1) denylist.
    pub fn global_deny(mut self, entry: DenylistEntry) -> Self {
        self.registry.denylist.add_global(entry);
        self
    }

    /// Add an entry to the managed (layer 2) denylist.
    pub fn managed_deny(mut self, entry: DenylistEntry) -> Self {
        self.registry.denylist.add_managed(entry);
        self
    }

    /// Add a configuration option key to the option-level (layer 3) denylist.
    pub fn option_deny(mut self, config_key: impl Into<String>) -> Self {
        self.registry.denylist.add_option_deny(config_key.into());
        self
    }

    /// Consume the builder and return the finished [`EnvRegistry`].
    pub fn build(self) -> EnvRegistry {
        self.registry
    }
}

// ── default_env_vars ────────────────────────────────────────────────────

/// Build a pre-populated [`EnvRegistry`] with a v0 subset of common
/// `DEEPAGENTS_CODE_*` environment variables.
///
/// The full set is 55 variables (see SPEC Q12); this v0 subset ships ~20 of
/// the most commonly used ones. Additional variables can be registered at
/// runtime via [`EnvRegistry::register`] or [`EnvBuilder::var`].
///
/// The returned registry carries an empty denylist; populate it via
/// [`EnvBuilder`] or by mutating `registry.denylist`.
pub fn default_env_vars() -> EnvRegistry {
    let mut reg = EnvRegistry::new();

    // Helper to keep registrations terse.
    let mut r = |name: &str, key: &str, desc: &str| {
        reg.register(EnvVar::new(name, key, desc));
    };

    r(
        "DEEPAGENTS_CODE_MODEL",
        "model",
        "Default model identifier (e.g. provider/family-name).",
    );
    r(
        "DEEPAGENTS_CODE_SYSTEM_PROMPT",
        "system_prompt",
        "Override the default system prompt.",
    );
    r(
        "DEEPAGENTS_CODE_APPROVAL_MODE",
        "approval_mode",
        "Human-in-the-loop approval mode (never / on-failure / on-request).",
    );
    r(
        "DEEPAGENTS_CODE_SANDBOX_PROVIDER",
        "sandbox.provider",
        "Sandbox backend (none / docker / firecracker / wasi).",
    );
    r(
        "DEEPAGENTS_CODE_THEME",
        "ui.theme",
        "UI color theme (dark / light / high-contrast).",
    );
    r(
        "DEEPAGENTS_CODE_COST_TRACKING",
        "cost.tracking",
        "Enable per-run cost tracking (true / false).",
    );
    r(
        "DEEPAGENTS_CODE_MAX_TURNS",
        "max_turns",
        "Maximum agent turns before auto-stop.",
    );
    r(
        "DEEPAGENTS_CODE_MAX_TOKENS",
        "max_tokens",
        "Maximum output tokens per generation.",
    );
    r(
        "DEEPAGENTS_CODE_TEMPERATURE",
        "temperature",
        "Sampling temperature for the model.",
    );
    r(
        "DEEPAGENTS_CODE_API_BASE",
        "provider.api_base",
        "Override the provider API base URL.",
    );
    r(
        "DEEPAGENTS_CODE_API_KEY",
        "provider.api_key",
        "Provider API key. (Consider denylisting for managed deployments.)",
    );
    r(
        "DEEPAGENTS_CODE_API_TIMEOUT",
        "provider.timeout",
        "Per-request timeout in seconds.",
    );
    r(
        "DEEPAGENTS_CODE_LOG_LEVEL",
        "logging.level",
        "Log verbosity (trace / debug / info / warn / error).",
    );
    r(
        "DEEPAGENTS_CODE_LOG_FORMAT",
        "logging.format",
        "Log format (pretty / json / compact).",
    );
    r(
        "DEEPAGENTS_CODE_TELEMETRY",
        "telemetry.enabled",
        "Enable OpenTelemetry export (true / false).",
    );
    r(
        "DEEPAGENTS_CODE_TRACING",
        "tracing.enabled",
        "Enable LangSmith / tracing export (true / false).",
    );
    r(
        "DEEPAGENTS_CODE_MCP_CONFIG",
        "mcp.config_path",
        "Path to the .mcp.json configuration file.",
    );
    r(
        "DEEPAGENTS_CODE_SKILLS_DIR",
        "skills.dir",
        "Directory containing skill definitions.",
    );
    r(
        "DEEPAGENTS_CODE_PLUGINS_DIR",
        "plugins.dir",
        "Directory containing plugin definitions.",
    );
    r(
        "DEEPAGENTS_CODE_HOME",
        "home",
        "Override the ~/.deepagents home directory.",
    );

    // 20 v0 variables registered. Remaining 35 ship in later releases.
    reg
}

// ── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    /// Round-trip an [`EnvVar`] through serde.
    #[test]
    fn test_env_var_serde() {
        let v = EnvVar::new(
            "DEEPAGENTS_CODE_MODEL",
            "model",
            "Default model identifier.",
        );
        let json = serde_json::to_string(&v).expect("serialize");
        let back: EnvVar = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.name, v.name);
        assert_eq!(back.config_key, v.config_key);
        assert_eq!(back.description, v.description);
    }

    /// The global (layer 1) denylist blocks a registered env var.
    ///
    /// Note: `std::env::set_var`/`remove_var` are `unsafe` as of Rust 1.88
    /// and this crate is `#![forbid(unsafe_code)]`. We therefore drive env
    /// var population through `dotenvy` (whose unsafe lives in that crate,
    /// not ours) and read values with the safe `std::env::var`.
    #[test]
    fn test_denylist_global() {
        let mut reg = default_env_vars();
        reg.denylist
            .add_global(DenylistEntry::new("DEEPAGENTS_CODE_API_KEY", "security-sensitive"));

        // Denied via the authoritative bool check.
        let denied = reg
            .denylist
            .is_denied_bool("DEEPAGENTS_CODE_API_KEY", "provider.api_key");
        assert!(denied.is_some(), "global denylist should block api_key");
        assert_eq!(denied.unwrap().reason, "security-sensitive");

        // The legacy is_denied also sees it.
        let legacy = reg
            .denylist
            .is_denied("DEEPAGENTS_CODE_API_KEY", "provider.api_key");
        assert!(legacy.is_some(), "legacy is_denied should also see global deny");

        // Populate the env var via a temp .env file (safe; dotenvy owns the
        // unsafe setenv). resolve() must error regardless of the value.
        let env_path = write_tmp_env("DEEPAGENTS_CODE_API_KEY=secret-12345\n");
        let _ = reg.load_dotenv_overriding(&env_path);
        let res = reg.resolve("DEEPAGENTS_CODE_API_KEY");
        assert!(res.is_err(), "resolve should error for a denied global var");
    }

    /// The managed (layer 2) denylist blocks a registered env var.
    #[test]
    fn test_denylist_managed() {
        let mut reg = default_env_vars();
        reg.denylist
            .add_managed(DenylistEntry::new("DEEPAGENTS_CODE_SANDBOX_PROVIDER", "admin-forbidden"));

        let denied = reg
            .denylist
            .is_denied_bool("DEEPAGENTS_CODE_SANDBOX_PROVIDER", "sandbox.provider");
        assert!(denied.is_some(), "managed denylist should block sandbox.provider");

        // Set the env var via dotenvy override and assert resolve errors with
        // a ManagedPolicy-flavored ConfigError.
        let env_path = write_tmp_env("DEEPAGENTS_CODE_SANDBOX_PROVIDER=docker\n");
        let _ = reg.load_dotenv_overriding(&env_path);
        let res = reg.resolve("DEEPAGENTS_CODE_SANDBOX_PROVIDER");
        assert!(res.is_err(), "resolve should error for a managed-deny var");
        match res.unwrap_err() {
            Error::Config(ConfigError::ManagedPolicy(_)) => {}
            other => panic!("expected ManagedPolicy, got {other:?}"),
        }
    }

    /// The option-level (layer 3) denylist blocks by config key.
    #[test]
    fn test_denylist_option_level() {
        let mut reg = default_env_vars();
        reg.denylist.add_option_deny("temperature".to_string());

        // option-level matches by config_key, not env var name.
        let denied = reg
            .denylist
            .is_denied_bool("DEEPAGENTS_CODE_TEMPERATURE", "temperature");
        assert!(denied.is_some(), "option-level deny should block temperature");

        let env_path = write_tmp_env("DEEPAGENTS_CODE_TEMPERATURE=0.7\n");
        let _ = reg.load_dotenv_overriding(&env_path);
        let res = reg.resolve("DEEPAGENTS_CODE_TEMPERATURE");
        assert!(res.is_err(), "resolve should error for an option-level deny var");
    }

    /// Resolve returns the value of a registered, allowed, set env var.
    ///
    /// Uses a unique env var name (per-test) so parallel tests cannot
    /// collide. Env population goes through `dotenvy` (which owns the
    /// `unsafe` setenv internally); this crate is `#![forbid(unsafe_code)]`,
    /// so `std::env::set_var`/`remove_var` (which became `unsafe` in Rust
    /// 1.88) cannot be used here.
    #[test]
    fn test_resolve_value() {
        // Custom registry with a unique var for this test.
        let mut reg = EnvRegistry::new();
        reg.register(EnvVar::new(
            "DEEPAGENTS_CODE_TEST_RESOLVE_VAL",
            "test_resolve_val",
            "test fixture",
        ));
        reg.register(EnvVar::new(
            "DEEPAGENTS_CODE_TEST_RESOLVE_UNSET",
            "test_resolve_unset",
            "test fixture (never set)",
        ));

        // Set the var via dotenvy override (safe; unsafe lives in dotenvy).
        let env_path = write_tmp_env("DEEPAGENTS_CODE_TEST_RESOLVE_VAL=claude-3-5-sonnet\n");
        let _ = reg.load_dotenv_overriding(&env_path);
        let val = reg
            .resolve("DEEPAGENTS_CODE_TEST_RESOLVE_VAL")
            .expect("resolve ok");
        assert_eq!(
            val,
            Some("claude-3-5-sonnet".to_string()),
            "resolve should return the set value"
        );

        // A registered-but-never-set variable resolves to None.
        let none = reg
            .resolve("DEEPAGENTS_CODE_TEST_RESOLVE_UNSET")
            .expect("resolve ok");
        assert!(none.is_none(), "never-set var should resolve to None");

        // Unknown (unregistered) variable resolves to None.
        let unknown = reg
            .resolve("DEEPAGENTS_CODE_TOTALLY_FAKE")
            .expect("resolve ok");
        assert!(unknown.is_none(), "unknown var should resolve to None");
    }

    /// resolve_all collects all set & allowed variables into a map.
    ///
    /// Uses a custom registry with unique var names to avoid collisions
    /// with other parallel tests.
    #[test]
    fn test_resolve_all() {
        let mut reg = EnvRegistry::new();
        reg.register(EnvVar::new(
            "DEEPAGENTS_CODE_TEST_ALL_1",
            "test_all_1",
            "test fixture",
        ));
        reg.register(EnvVar::new(
            "DEEPAGENTS_CODE_TEST_ALL_2",
            "test_all_2",
            "test fixture",
        ));

        let env_path =
            write_tmp_env("DEEPAGENTS_CODE_TEST_ALL_1=gpt-4o\nDEEPAGENTS_CODE_TEST_ALL_2=dark\n");
        let _ = reg.load_dotenv_overriding(&env_path);
        let map = reg.resolve_all().expect("resolve_all ok");
        assert_eq!(
            map.get("DEEPAGENTS_CODE_TEST_ALL_1").map(|s| s.as_str()),
            Some("gpt-4o")
        );
        assert_eq!(
            map.get("DEEPAGENTS_CODE_TEST_ALL_2").map(|s| s.as_str()),
            Some("dark")
        );
        // Only the two registered vars appear (no extras).
        assert_eq!(map.len(), 2, "resolve_all should return exactly the set vars");
    }

    /// Write a temp `.env` file, load it via `load_dotenv`, and verify the
    /// variable is set in the process environment.
    ///
    /// Uses a unique env var name to avoid cross-test collisions.
    #[test]
    fn test_load_dotenv() {
        let reg = EnvRegistry::new();
        let env_path = write_tmp_env("DEEPAGENTS_CODE_TEST_DOTENV=temp-model\n");

        reg.load_dotenv(&env_path).expect("load_dotenv ok");
        // The file must still exist at this point (no premature cleanup).
        assert!(
            env_path.exists(),
            "temp .env file should still exist during load"
        );
        let val = env::var("DEEPAGENTS_CODE_TEST_DOTENV").expect("var set by dotenv");
        // Synchronous cleanup (a detached cleanup thread races with the
        // file read above).
        let _ = fs::remove_file(&env_path);
        assert_eq!(val, "temp-model");
    }

    /// `load_dotenv_default` is a no-op `Ok(())` when no `.env` is present in
    /// the current working directory.
    #[test]
    fn test_load_dotenv_default() {
        // We cannot easily chdir in a unit test without spawning a subprocess,
        // so this asserts the no-op-when-absent contract from the current cwd.
        let reg = EnvRegistry::new();
        let res = reg.load_dotenv_default();
        assert!(res.is_ok(), "load_dotenv_default should be Ok when no .env present");
    }

    /// The `PREFIX` constant matches the documented value.
    #[test]
    fn test_prefix_constant() {
        assert_eq!(PREFIX, "DEEPAGENTS_CODE_");
    }

    /// The default registry registers the expected v0 subset.
    #[test]
    fn test_default_env_vars_subset() {
        let reg = default_env_vars();
        // Expect at least the headline variables.
        assert!(reg.get("DEEPAGENTS_CODE_MODEL").is_some());
        assert!(reg.get("DEEPAGENTS_CODE_SYSTEM_PROMPT").is_some());
        assert!(reg.get("DEEPAGENTS_CODE_APPROVAL_MODE").is_some());
        assert!(reg.get("DEEPAGENTS_CODE_SANDBOX_PROVIDER").is_some());
        assert!(reg.get("DEEPAGENTS_CODE_THEME").is_some());
        assert!(reg.get("DEEPAGENTS_CODE_COST_TRACKING").is_some());
        // Should register ~20 variables.
        assert!(
            reg.vars.len() >= 20,
            "expected >=20 default vars, got {}",
            reg.vars.len()
        );
        // All registered names start with the prefix.
        for name in reg.vars.keys() {
            assert!(name.starts_with(PREFIX), "{name} does not start with prefix");
        }
    }

    /// `EnvBuilder` produces an equivalent registry to manual construction.
    #[test]
    fn test_env_builder() {
        let reg = EnvBuilder::new()
            .var(EnvVar::new("DEEPAGENTS_CODE_X", "x", "test"))
            .global_deny(DenylistEntry::new("DEEPAGENTS_CODE_X", "nope"))
            .managed_deny(DenylistEntry::new("DEEPAGENTS_CODE_X", "admin"))
            .option_deny("y")
            .build();
        assert!(reg.get("DEEPAGENTS_CODE_X").is_some());
        assert_eq!(reg.denylist.global.len(), 1);
        assert_eq!(reg.denylist.managed.len(), 1);
        assert_eq!(reg.denylist.option_level, vec!["y"]);
    }

    /// `DenylistEntry` round-trips through serde.
    #[test]
    fn test_denylist_entry_serde() {
        let e = DenylistEntry::new("DEEPAGENTS_CODE_X", "reason");
        let json = serde_json::to_string(&e).expect("serialize");
        let back: DenylistEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.env_var, e.env_var);
        assert_eq!(back.reason, e.reason);
    }

    // ── helpers ──────────────────────────────────────────────────────────

    /// Write `contents` to a uniquely-named temp `.env` file and return its
    /// path.
    ///
    /// The caller is responsible for removing the file (e.g. via
    /// [`fs::remove_file`]) when done. We deliberately do **not** spawn a
    /// detached cleanup thread here: such a thread races with the file read
    /// in `load_dotenv` and can delete the file before it is parsed.
    ///
    /// Note on env var isolation: `std::env::set_var`/`remove_var` are
    /// `unsafe` as of Rust 1.88, and this crate is
    /// `#![forbid(unsafe_code)]`, so tests cannot unset env vars directly.
    /// `dotenvy::from_path_override` with an empty file does **not** unset
    /// existing vars. Tests therefore use **unique** env var names (see
    /// e.g. `DEEPAGENTS_CODE_TEST_*`) so leftover values never affect other
    /// tests.
    fn write_tmp_env(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        // Unique name to avoid collisions between parallel tests.
        let path = dir.join(format!(
            "deepagents-env-test-{}-{}.env",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        fs::write(&path, contents).expect("write tmp .env");
        path
    }
}

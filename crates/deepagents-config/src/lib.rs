//! config.toml 6-layer ranked resolver, 105 options (Q11)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.
//!
//! This crate implements the six-layer configuration resolver described in the
//! Deep Agents specification. Configuration values are ranked from the
//! highest-priority ephemeral source (CLI flags, rank 0) down to the immutable
//! built-in defaults (rank 5). Each layer may be independently loaded from a
//! `config.toml` file, programmatically, or merged at runtime, and the resolver
//! walks the ranks from highest (0) to lowest (5), applying each option's
//! declared merge strategy.
//!
//! The catalog of declared options (`ConfigOption`) is the single source of
//! truth for an option's key, type, default, env var, CLI flag, merge strategy,
//! and human description. Values that do not match a declared option are
//! rejected and recorded in the tier diagnostics so callers can surface "you
//! typed `model` but meant `ai.model`" style messages.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use deepagents_errors::{ConfigError, Error};
use serde::{Deserialize, Serialize};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── Value model ──────────────────────────────────────────────────────────

/// A typed configuration value.
///
/// This is the runtime representation of any option value, regardless of which
/// layer it came from. It mirrors the subset of TOML/JSON types that the
/// resolver needs to support, and is serializable so resolved registries can
/// be persisted or emitted as diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    /// A string value, e.g. a model name or prompt template.
    String(String),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit floating point number.
    Float(f64),
    /// A boolean flag.
    Bool(bool),
    /// An ordered array of values.
    Array(Vec<ConfigValue>),
    /// A key/value map (table), stored as a `BTreeMap` so that serialization
    /// is deterministic and merged tables have a stable key order.
    Table(BTreeMap<String, ConfigValue>),
}

impl ConfigValue {
    /// Returns `true` if this value is a [`ConfigValue::String`].
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    /// Returns the inner string if this is a [`ConfigValue::String`].
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

// ─── Declared type ────────────────────────────────────────────────────────

/// The declared type of a `ConfigOption`.
///
/// This describes the *expected* type of a value, as opposed to the runtime
/// [`ConfigValue`] which is the concrete value. Type checking happens when a
/// value is set at a tier: a value whose variant does not match the option's
/// declared type is rejected and recorded in the tier diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigType {
    /// String type.
    String,
    /// Integer type.
    Int,
    /// Floating point type.
    Float,
    /// Boolean type.
    Bool,
    /// Array type.
    Array,
    /// Table type.
    Table,
}

impl ConfigType {
    /// Returns `true` if a [`ConfigValue`] matches this declared type.
    pub fn matches(&self, value: &ConfigValue) -> bool {
        match (self, value) {
            (Self::String, ConfigValue::String(_)) => true,
            (Self::Int, ConfigValue::Int(_)) => true,
            (Self::Float, ConfigValue::Float(_)) => true,
            (Self::Bool, ConfigValue::Bool(_)) => true,
            (Self::Array, ConfigValue::Array(_)) => true,
            (Self::Table, ConfigValue::Table(_)) => true,
            _ => false,
        }
    }

    /// Returns a human-readable name for this type.
    pub fn name(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::Array => "array",
            Self::Table => "table",
        }
    }
}

// ─── Merge strategy ───────────────────────────────────────────────────────

/// How a higher-rank value combines with a lower-rank one during resolution.
///
/// The resolver walks ranks from highest (0) to lowest (5). When it encounters
/// a value at a given rank, it consults the option's merge strategy to decide
/// whether the new value completely replaces the accumulated value or is
/// deep-merged into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// The higher-rank value fully replaces the lower-rank value.
    ///
    /// For scalar types this is the only sensible behavior; for `Array` it
    /// replaces the entire array.
    Replace,
    /// Deep-merge the higher-rank value into the lower-rank value.
    ///
    /// For `Table` values this recursively merges keys (higher-rank keys win
    /// on conflict). For `Array` values the higher-rank array is appended to
    /// the lower-rank one. For scalars this behaves like [`Self::Replace`].
    Merge,
}

/// Deep-merge `higher` into `lower`, returning the merged value.
///
/// The `lower` value is consumed and updated in place where possible.
fn merge_values(lower: ConfigValue, higher: &ConfigValue) -> ConfigValue {
    match (lower, higher) {
        // Tables merge recursively.
        (ConfigValue::Table(mut lo), ConfigValue::Table(hi)) => {
            for (k, v) in hi {
                // Recursively merge if both existing and incoming are tables;
                // otherwise the higher-rank value replaces the lower-rank one.
                let merged = if let Some(ConfigValue::Table(_)) = lo.get(k) {
                    if let ConfigValue::Table(_) = v {
                        let taken = lo.remove(k).expect("checked above");
                        Some(merge_values(taken, v))
                    } else {
                        Some(v.clone())
                    }
                } else {
                    Some(v.clone())
                };
                lo.insert(k.to_string(), merged.expect("value always Some"));
            }
            ConfigValue::Table(lo)
        }
        // Arrays are appended (higher-rank values come last).
        (ConfigValue::Array(mut lo), ConfigValue::Array(hi)) => {
            lo.extend(hi.iter().cloned());
            ConfigValue::Array(lo)
        }
        // Everything else: higher fully replaces lower.
        (_, higher) => higher.clone(),
    }
}

// ─── Rank ─────────────────────────────────────────────────────────────────

/// The priority rank of a configuration source.
///
/// Rank 0 is the highest priority (CLI flags, ephemeral) and rank 5 is the
/// lowest priority (built-in defaults, immutable). The resolver walks ranks in
/// ascending order (0 → 5) so that the first value encountered at the highest
/// rank wins, subject to the option's merge strategy.
///
/// This mirrors the six-layer ranked resolver from the Q11 spec.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rank {
    /// Rank 0 — CLI flags (ephemeral).
    Cli = 0,
    /// Rank 1 — environment variables (ephemeral).
    Env = 1,
    /// Rank 2 — managed config / admin policy (durable).
    Managed = 2,
    /// Rank 3 — project config `./deepagents/config.toml` (semi-durable).
    Project = 3,
    /// Rank 4 — user config `~/.deepagents/config.toml` (durable).
    User = 4,
    /// Rank 5 — built-in defaults (immutable).
    Defaults = 5,
}

impl Rank {
    /// All ranks, ordered from highest (0) to lowest (5) priority.
    pub const ALL: [Rank; 6] = [
        Rank::Cli,
        Rank::Env,
        Rank::Managed,
        Rank::Project,
        Rank::User,
        Rank::Defaults,
    ];

    /// A short human-readable label for this rank.
    pub fn label(self) -> &'static str {
        match self {
            Rank::Cli => "cli",
            Rank::Env => "env",
            Rank::Managed => "managed",
            Rank::Project => "project",
            Rank::User => "user",
            Rank::Defaults => "defaults",
        }
    }
}

impl std::fmt::Display for Rank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ─── Source & diagnostics ─────────────────────────────────────────────────

/// Describes where a resolved value came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSource {
    /// The priority rank of the contributing layer.
    pub rank: Rank,
    /// A human-readable label for the specific source (e.g. `~/.deepagents/config.toml`).
    pub label: String,
}

/// Per-tier rejection reasons.
///
/// A newtype wrapper around `HashMap<Rank, Vec<String>>` that centralizes the
/// logic for appending a rejection reason at a given tier. Keys are
/// [`Rank`]s; values are the list of reasons values were rejected at that tier
/// (e.g. unknown key, type mismatch). This is the structured form of the
/// "tier_diagnostics" table from the Q11 spec.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TierDiagnostics {
    /// The inner map: rank → list of rejection reasons.
    inner: HashMap<Rank, Vec<String>>,
}

impl TierDiagnostics {
    /// Create an empty diagnostics map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a rejection reason at the given rank.
    ///
    /// This is the single entry point used by the resolver whenever a value is
    /// rejected (unknown key, type mismatch, unsupported TOML value, …).
    pub fn add(&mut self, rank: Rank, reason: impl Into<String>) {
        self.inner.entry(rank).or_default().push(reason.into());
    }

    /// Returns the rejection reasons recorded at `rank`, if any.
    pub fn get(&self, rank: Rank) -> Option<&[String]> {
        self.inner.get(&rank).map(|v| v.as_slice())
    }

    /// Returns an iterator over all (rank, reasons) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&Rank, &Vec<String>)> {
        self.inner.iter()
    }

    /// Returns `true` if there are no rejection reasons recorded at any rank.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// A file-wide diagnostic, emitted at most once per generation.
///
/// These describe situations that affect a whole source file rather than a
/// single key, as specified by the Q11 `SHADOWED_TABLE` / `UNUSABLE_SOURCE` /
/// `RETAINED_SOURCE` diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail")]
#[serde(rename_all = "snake_case")]
pub enum FileDiagnostic {
    /// A table was entirely shadowed by a higher-rank source (emitted once
    /// per generation). The detail names the shadowed table.
    ShadowedTable(String),
    /// A source file was entirely unusable (e.g. parse failure that we
    /// recovered from by skipping). The detail describes why.
    UnusableSource(String),
    /// A source file was retained despite partial issues (e.g. some keys
    /// rejected but the rest kept). The detail describes what was retained.
    RetainedSource(String),
}

// ─── Resolved value ───────────────────────────────────────────────────────

/// A fully resolved configuration value along with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedValue {
    /// The final (possibly merged) value.
    pub value: ConfigValue,
    /// Where the value ultimately came from (the highest contributing rank).
    pub source: ConfigSource,
}

// ─── Option definition ────────────────────────────────────────────────────

/// The declaration of a single configuration option.
///
/// The catalog of options is the single source of truth for what keys exist,
/// what types they accept, what their defaults are, and how they merge across
/// the six ranked layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigOption {
    /// The dotted option key, e.g. `ai.model`.
    pub key: String,
    /// The declared type of the option's value.
    #[serde(rename = "type")]
    pub r#type: ConfigType,
    /// The built-in default value (rank 5).
    pub default: ConfigValue,
    /// The environment variable name that maps to this option, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_name: Option<String>,
    /// The CLI flag that maps to this option, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_flag: Option<String>,
    /// How values for this option merge across ranks.
    #[serde(rename = "merge")]
    pub merge: MergeStrategy,
    /// A human-readable description of the option.
    pub description: String,
}

// ─── Registry ─────────────────────────────────────────────────────────────

/// The central configuration registry: option catalog + tiered values +
/// diagnostics.
///
/// A registry is built up by registering option definitions
/// ([`ConfigOption`]) and then setting values at one or more ranks
/// ([`Rank`]). The [`ConfigRegistry::resolve`] and
/// [`ConfigRegistry::resolve_all`] methods walk the ranks from highest (0) to
/// lowest (5) priority, applying each option's merge strategy, to produce the
/// final [`ResolvedValue`]s.
#[derive(Debug, Clone, Default)]
pub struct ConfigRegistry {
    /// The declared option catalog, keyed by option key.
    options: HashMap<String, ConfigOption>,
    /// Tiered values: rank → (key → value). Only ranks with at least one
    /// value are present.
    tiers: HashMap<Rank, HashMap<String, ConfigValue>>,
    /// Per-tier rejection reasons.
    tier_diags: TierDiagnostics,
    /// File-wide diagnostics, in insertion order.
    file_diags: Vec<FileDiagnostic>,
}

impl ConfigRegistry {
    /// Create an empty registry with no options and no values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an option definition.
    ///
    /// If an option with the same key already exists it is replaced.
    pub fn register(&mut self, option: ConfigOption) {
        self.options.insert(option.key.clone(), option);
    }

    /// Returns the declared option for `key`, if any.
    pub fn option(&self, key: &str) -> Option<&ConfigOption> {
        self.options.get(key)
    }

    /// Returns an iterator over all declared options.
    pub fn options(&self) -> impl Iterator<Item = &ConfigOption> {
        self.options.values()
    }

    /// Returns the number of declared options.
    pub fn option_count(&self) -> usize {
        self.options.len()
    }

    /// Set a value at a given tier.
    ///
    /// If the key is not a known option, the value is rejected and a reason
    /// is recorded in the tier diagnostics. If the value's type does not match
    /// the option's declared type, it is likewise rejected. On success the
    /// value is stored at the given rank, replacing any previous value at
    /// that rank for the same key.
    pub fn set_value(&mut self, rank: Rank, key: &str, value: ConfigValue) {
        // Validate against the declared catalog.
        let Some(option) = self.options.get(key) else {
            self.tier_diags
                .add(rank, format!("unknown option key `{key}`"));
            return;
        };
        if !option.r#type.matches(&value) {
            self.tier_diags.add(rank, format!(
                "option `{key}` expects type `{}`, got `{}`",
                option.r#type.name(),
                value_variant_name(&value)
            ));
            return;
        }
        self.tiers
            .entry(rank)
            .or_default()
            .insert(key.to_string(), value);
    }

    /// Resolve a single option by walking ranks 0 → 5.
    ///
    /// Returns `None` if the option is not declared. Otherwise the resolver
    /// walks ranks from highest (0) to lowest (5) priority. For each rank
    /// that has a value for the key, it accumulates the value according to the
    /// option's merge strategy: the first (highest-rank) value seeds the
    /// accumulation; subsequent lower-rank values either replace or deep-merge
    /// depending on the strategy. The result's [`ConfigSource`] records the
    /// highest contributing rank.
    pub fn resolve(&self, key: &str) -> Option<ResolvedValue> {
        let option = self.options.get(key)?;
        let mut acc: Option<(ConfigValue, Rank)> = None;

        for &rank in &Rank::ALL {
            if let Some(tier) = self.tiers.get(&rank) {
                if let Some(value) = tier.get(key) {
                    match (acc.take(), option.merge) {
                        (None, _) => {
                            acc = Some((value.clone(), rank));
                        }
                        (Some((existing, existing_rank)), MergeStrategy::Replace) => {
                            // Higher-rank (smaller number) value already present;
                            // it fully shadows the lower-rank value we just found.
                            acc = Some((existing, existing_rank));
                        }
                        (Some((existing, existing_rank)), MergeStrategy::Merge) => {
                            // We are walking high→low. `existing` is the
                            // higher-priority (winning) value; `value` is
                            // lower-priority. Deep-merge the lower into the
                            // higher so that higher-rank keys win on conflict.
                            let merged = merge_values(value.clone(), &existing);
                            acc = Some((merged, existing_rank));
                        }
                    }
                }
            }
        }

        acc.map(|(value, rank)| ResolvedValue {
            value,
            source: ConfigSource {
                rank,
                label: rank.label().to_string(),
            },
        })
    }

    /// Resolve every declared option.
    ///
    /// Options that resolve to nothing (no value at any rank) are omitted.
    /// Note that defaults are *not* automatically loaded here; callers should
    /// either call [`ConfigRegistry::load_defaults`] or seed rank 5 manually.
    pub fn resolve_all(&self) -> HashMap<String, ResolvedValue> {
        self.options
            .keys()
            .filter_map(|key| self.resolve(key).map(|rv| (key.clone(), rv)))
            .collect()
    }

    /// Returns the per-tier rejection reasons.
    pub fn tier_diagnostics(&self) -> &TierDiagnostics {
        &self.tier_diags
    }

    /// Returns the file-wide diagnostics, in insertion order.
    pub fn file_diagnostics(&self) -> &[FileDiagnostic] {
        &self.file_diags
    }

    /// Load the built-in defaults (rank 5) for all declared options.
    ///
    /// This sets every option's `default` value at [`Rank::Defaults`]. It is
    /// idempotent: calling it twice simply overwrites the defaults tier.
    pub fn load_defaults(&mut self) {
        let defaults: Vec<(String, ConfigValue)> = self
            .options
            .values()
            .map(|o| (o.key.clone(), o.default.clone()))
            .collect();
        let tier = self.tiers.entry(Rank::Defaults).or_default();
        for (k, v) in defaults {
            tier.insert(k, v);
        }
    }

    /// Parse a TOML string and set all values at the given rank.
    ///
    /// The TOML is flattened into dotted keys (e.g. `[ai]` with `model = "x"`
    /// becomes the key `ai.model`). Unknown keys are rejected and recorded in
    /// the tier diagnostics; the rest are kept and a
    /// [`FileDiagnostic::RetainedSource`] is emitted. On a hard parse failure
    /// an [`ConfigError::Load`] is returned and an
    /// [`FileDiagnostic::UnusableSource`] is recorded.
    pub fn load_toml(&mut self, rank: Rank, content: &str) -> Result<(), Error> {
        let parsed: toml::Value = match toml::from_str(content) {
            Ok(v) => v,
            Err(e) => {
                self.file_diags
                    .push(FileDiagnostic::UnusableSource(format!(
                        "rank {rank} toml parse error: {e}"
                    )));
                return Err(ConfigError::Load(format!(
                    "rank {rank} toml parse error: {e}"
                ))
                .into());
            }
        };

        let mut retained = 0usize;
        let mut rejected = 0usize;
        for (k, v) in flatten_toml(&parsed) {
            if let Some(cv) = toml_to_config_value(&v) {
                let before_rejected = self
                    .tier_diags
                    .get(rank)
                    .map(|v| v.len())
                    .unwrap_or(0);
                self.set_value(rank, &k, cv);
                let after_rejected = self
                    .tier_diags
                    .get(rank)
                    .map(|v| v.len())
                    .unwrap_or(0);
                if after_rejected > before_rejected {
                    rejected += 1;
                } else {
                    retained += 1;
                }
            } else {
                self.tier_diags
                    .add(rank, format!("option `{k}` has unsupported toml value"));
                rejected += 1;
            }
        }

        if retained > 0 && rejected > 0 {
            self.file_diags
                .push(FileDiagnostic::RetainedSource(format!(
                    "rank {rank}: retained {retained} key(s), rejected {rejected}"
                )));
        }
        Ok(())
    }

    /// Load a TOML file at the given rank.
    ///
    /// Reads the file at `path` and delegates to [`ConfigRegistry::load_toml`].
    pub fn load_toml_file(&mut self, rank: Rank, path: &str) -> Result<(), Error> {
        let content = std::fs::read_to_string(Path::new(path)).map_err(|e| {
            ConfigError::Load(format!("rank {rank}: cannot read {path}: {e}"))
        })?;
        self.load_toml(rank, &content)
    }
}

/// Returns the human-readable variant name of a [`ConfigValue`].
fn value_variant_name(value: &ConfigValue) -> &'static str {
    match value {
        ConfigValue::String(_) => "string",
        ConfigValue::Int(_) => "int",
        ConfigValue::Float(_) => "float",
        ConfigValue::Bool(_) => "bool",
        ConfigValue::Array(_) => "array",
        ConfigValue::Table(_) => "table",
    }
}

/// Flatten a TOML value into a list of (dotted-key, value) pairs.
///
/// Tables are recursed into with dotted keys; arrays and scalars are kept
/// verbatim. The top-level value must be a table.
fn flatten_toml(value: &toml::Value) -> Vec<(String, toml::Value)> {
    let mut out = Vec::new();
    if let toml::Value::Table(table) = value {
        for (k, v) in table {
            flatten_toml_inner(k, v, &mut out);
        }
    }
    out
}

fn flatten_toml_inner(prefix: &str, value: &toml::Value, out: &mut Vec<(String, toml::Value)>) {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                let next = format!("{prefix}.{k}");
                flatten_toml_inner(&next, v, out);
            }
        }
        other => {
            out.push((prefix.to_string(), other.clone()));
        }
    }
}

/// Convert a TOML value into a [`ConfigValue`], if possible.
///
/// `toml::Value` variants that have no [`ConfigValue`] equivalent (datetime)
/// return `None`.
fn toml_to_config_value(value: &toml::Value) -> Option<ConfigValue> {
    match value {
        toml::Value::String(s) => Some(ConfigValue::String(s.clone())),
        toml::Value::Integer(i) => Some(ConfigValue::Int(*i)),
        toml::Value::Float(f) => Some(ConfigValue::Float(*f)),
        toml::Value::Boolean(b) => Some(ConfigValue::Bool(*b)),
        toml::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(toml_to_config_value(v)?);
            }
            Some(ConfigValue::Array(out))
        }
        toml::Value::Table(t) => {
            let mut out = BTreeMap::new();
            for (k, v) in t {
                out.insert(k.clone(), toml_to_config_value(v)?);
            }
            Some(ConfigValue::Table(out))
        }
        // Datetime variants have no ConfigValue representation.
        _ => None,
    }
}

// ─── Builder ──────────────────────────────────────────────────────────────

/// Builder for constructing a [`ConfigRegistry`] programmatically.
///
/// The builder starts with an empty registry. Options are registered with
/// [`ConfigBuilder::option`], values are set with
/// [`ConfigBuilder::set`], and the final registry is produced with
/// [`ConfigBuilder::build`]. The builder consumes itself on `build`.
#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    registry: ConfigRegistry,
}

impl ConfigBuilder {
    /// Create a new, empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an option definition.
    #[must_use]
    pub fn option(mut self, option: ConfigOption) -> Self {
        self.registry.register(option);
        self
    }

    /// Set a value at a rank.
    #[must_use]
    pub fn set(mut self, rank: Rank, key: &str, value: ConfigValue) -> Self {
        self.registry.set_value(rank, key, value);
        self
    }

    /// Load built-in defaults for all registered options.
    #[must_use]
    pub fn with_defaults(mut self) -> Self {
        self.registry.load_defaults();
        self
    }

    /// Load a TOML string at the given rank.
    ///
    /// Errors are propagated from [`ConfigRegistry::load_toml`].
    pub fn load_toml(mut self, rank: Rank, content: &str) -> Result<Self, Error> {
        self.registry.load_toml(rank, content)?;
        Ok(self)
    }

    /// Consume the builder and return the assembled registry.
    pub fn build(self) -> ConfigRegistry {
        self.registry
    }
}

// ─── Defaults catalog (v0 subset, ≥20 of ~105) ─────────────────────────────

/// Returns a [`ConfigRegistry`] pre-populated with a v0 subset of the ~105
/// built-in option declarations (at least 20 common ones) *and* their default
/// values already loaded at [`Rank::Defaults`].
///
/// This is the recommended starting point for most callers: it gives you a
/// registry that knows about the common options (`ai.model`, `ui.theme`,
/// permissions, sandbox, etc.) and resolves to sensible defaults out of the
/// box. Higher-rank sources (CLI, env, managed, project, user) can then be
/// layered on top.
pub fn defaults() -> ConfigRegistry {
    let mut registry = ConfigRegistry::new();

    // String options.
    for (key, default, env, cli, desc) in str_opts() {
        registry.register(str_option(key, default, env, cli, desc));
    }
    // Bool options.
    for (key, default, env, cli, desc) in bool_opts() {
        registry.register(bool_option(key, default, env, cli, desc));
    }
    // Int options.
    for (key, default, env, cli, desc) in int_opts() {
        registry.register(int_option(key, default, env, cli, desc));
    }
    // Float options.
    for (key, default, env, cli, desc) in float_opts() {
        registry.register(float_option(key, default, env, cli, desc));
    }
    // Array options.
    for (key, default, desc) in array_opts() {
        registry.register(array_option(key, &default, desc));
    }
    // Table options.
    for (key, default, desc) in table_opts() {
        registry.register(table_option(key, &default, desc));
    }

    // Load the defaults at rank 5.
    registry.load_defaults();
    registry
}

/// Build a string-typed option.
fn str_option(
    key: &str,
    default: &str,
    env: Option<&str>,
    cli: Option<&str>,
    desc: &str,
) -> ConfigOption {
    ConfigOption {
        key: key.to_string(),
        r#type: ConfigType::String,
        default: ConfigValue::String(default.to_string()),
        env_name: env.map(str::to_string),
        cli_flag: cli.map(str::to_string),
        merge: MergeStrategy::Replace,
        description: desc.to_string(),
    }
}

/// Build a bool-typed option.
fn bool_option(
    key: &str,
    default: bool,
    env: Option<&str>,
    cli: Option<&str>,
    desc: &str,
) -> ConfigOption {
    ConfigOption {
        key: key.to_string(),
        r#type: ConfigType::Bool,
        default: ConfigValue::Bool(default),
        env_name: env.map(str::to_string),
        cli_flag: cli.map(str::to_string),
        merge: MergeStrategy::Replace,
        description: desc.to_string(),
    }
}

/// Build an int-typed option.
fn int_option(
    key: &str,
    default: i64,
    env: Option<&str>,
    cli: Option<&str>,
    desc: &str,
) -> ConfigOption {
    ConfigOption {
        key: key.to_string(),
        r#type: ConfigType::Int,
        default: ConfigValue::Int(default),
        env_name: env.map(str::to_string),
        cli_flag: cli.map(str::to_string),
        merge: MergeStrategy::Replace,
        description: desc.to_string(),
    }
}

/// Build a float-typed option.
fn float_option(
    key: &str,
    default: f64,
    env: Option<&str>,
    cli: Option<&str>,
    desc: &str,
) -> ConfigOption {
    ConfigOption {
        key: key.to_string(),
        r#type: ConfigType::Float,
        default: ConfigValue::Float(default),
        env_name: env.map(str::to_string),
        cli_flag: cli.map(str::to_string),
        merge: MergeStrategy::Replace,
        description: desc.to_string(),
    }
}

/// Build an array-typed (mergeable) option.
fn array_option(key: &str, default: &[&str], desc: &str) -> ConfigOption {
    ConfigOption {
        key: key.to_string(),
        r#type: ConfigType::Array,
        default: ConfigValue::Array(
            default
                .iter()
                .map(|s| ConfigValue::String((*s).to_string()))
                .collect(),
        ),
        env_name: None,
        cli_flag: None,
        merge: MergeStrategy::Merge,
        description: desc.to_string(),
    }
}

/// Build a table-typed (mergeable) option.
fn table_option(key: &str, default: &[(&str, &str)], desc: &str) -> ConfigOption {
    let mut map = BTreeMap::new();
    for (k, v) in default {
        map.insert((*k).to_string(), ConfigValue::String((*v).to_string()));
    }
    ConfigOption {
        key: key.to_string(),
        r#type: ConfigType::Table,
        default: ConfigValue::Table(map),
        env_name: None,
        cli_flag: None,
        merge: MergeStrategy::Merge,
        description: desc.to_string(),
    }
}

/// The catalog of string-typed options in the v0 subset.
fn str_opts() -> Vec<(&'static str, &'static str, Option<&'static str>, Option<&'static str>, &'static str)> {
    vec![
        ("ai.model", "claude-3-5-sonnet", Some("DEEPAGENTS_MODEL"), Some("--model"), "Default LLM model id."),
        ("ai.provider", "anthropic", Some("DEEPAGENTS_PROVIDER"), Some("--provider"), "Default LLM provider."),
        ("ai.base_url", "", Some("DEEPAGENTS_BASE_URL"), None, "Override the provider base URL."),
        ("system_prompt", "", Some("DEEPAGENTS_SYSTEM_PROMPT"), None, "Override the system prompt."),
        ("approval_mode", "auto-edit", Some("DEEPAGENTS_APPROVAL_MODE"), Some("--approval-mode"), "Default approval mode."),
        ("sandbox.provider", "none", Some("DEEPAGENTS_SANDBOX"), Some("--sandbox"), "Sandbox provider."),
        ("ui.theme", "dark", Some("DEEPAGENTS_THEME"), Some("--theme"), "UI color theme."),
        ("ui.spinner", "dots", None, None, "Spinner animation style."),
        ("tracing.langsmith_api_key", "", Some("LANGSMITH_API_KEY"), None, "LangSmith API key."),
        ("tracing.project", "deepagents", Some("LANGSMITH_PROJECT"), None, "LangSmith project name."),
        ("session.storage", "~/.deepagents/sessions", None, None, "Session storage directory."),
        ("hooks.pre_tool", "", None, None, "Pre-tool hook command."),
        ("hooks.post_tool", "", None, None, "Post-tool hook command."),
    ]
}

/// The catalog of bool-typed options in the v0 subset.
fn bool_opts() -> Vec<(&'static str, bool, Option<&'static str>, Option<&'static str>, &'static str)> {
    vec![
        ("cost_tracking", true, Some("DEEPAGENTS_COST_TRACKING"), Some("--cost-tracking"), "Enable cost tracking."),
        ("ui.color", true, None, Some("--no-color"), "Enable colored output."),
        ("tracing.enabled", false, Some("DEEPAGENTS_TRACING"), None, "Enable LangSmith tracing."),
        ("session.compaction", true, None, None, "Enable context compaction."),
    ]
}

/// The catalog of int-typed options in the v0 subset.
fn int_opts() -> Vec<(&'static str, i64, Option<&'static str>, Option<&'static str>, &'static str)> {
    vec![
        ("ai.max_tokens", 8192, Some("DEEPAGENTS_MAX_TOKENS"), Some("--max-tokens"), "Maximum output tokens per response."),
        ("session.max_turns", 200, Some("DEEPAGENTS_MAX_TURNS"), Some("--max-turns"), "Maximum agent turns per session."),
    ]
}

/// The catalog of float-typed options in the v0 subset.
fn float_opts() -> Vec<(&'static str, f64, Option<&'static str>, Option<&'static str>, &'static str)> {
    vec![
        ("ai.temperature", 0.7, Some("DEEPAGENTS_TEMPERATURE"), Some("--temperature"), "Sampling temperature."),
    ]
}

/// The catalog of array-typed options in the v0 subset.
fn array_opts() -> Vec<(&'static str, &'static [&'static str], &'static str)> {
    vec![
        ("interrupt_on", &["tool_call"], "Events that interrupt the agent loop."),
        ("permissions.allow", &["read", "write"], "Permissions the agent is granted."),
        ("permissions.deny", &[], "Permissions the agent is denied."),
    ]
}

/// The catalog of table-typed options in the v0 subset.
fn table_opts() -> Vec<(&'static str, &'static [(&'static str, &'static str)], &'static str)> {
    vec![
        ("plugins", &[("auto_load", "true")], "Plugin configuration table (mergeable)."),
    ]
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! Tests for the config resolver.

    use super::*;

    /// Round-trip a `ConfigValue` through JSON and back.
    #[test]
    fn test_config_value_serde() {
        let mut table = BTreeMap::new();
        table.insert("model".to_string(), ConfigValue::String("gpt-4".to_string()));
        table.insert("temp".to_string(), ConfigValue::Float(0.5));
        let value = ConfigValue::Table(table);

        let json = serde_json::to_string(&value).expect("serialize");
        let back: ConfigValue = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(value, back);
    }

    /// With `MergeStrategy::Replace`, a higher-rank value fully replaces a
    /// lower-rank one.
    #[test]
    fn test_merge_strategy_replace() {
        let mut reg = ConfigRegistry::new();
        reg.register(ConfigOption {
            key: "ai.model".to_string(),
            r#type: ConfigType::String,
            default: ConfigValue::String("default-model".to_string()),
            env_name: None,
            cli_flag: None,
            merge: MergeStrategy::Replace,
            description: "model".to_string(),
        });
        reg.set_value(Rank::Defaults, "ai.model", ConfigValue::String("default-model".to_string()));
        reg.set_value(Rank::Cli, "ai.model", ConfigValue::String("cli-model".to_string()));

        let resolved = reg.resolve("ai.model").expect("should resolve");
        assert_eq!(
            resolved.value,
            ConfigValue::String("cli-model".to_string())
        );
        assert_eq!(resolved.source.rank, Rank::Cli);
    }

    /// With `MergeStrategy::Merge`, a higher-rank table deep-merges into a
    /// lower-rank table (higher-rank keys win on conflict).
    #[test]
    fn test_merge_strategy_merge() {
        let mut reg = ConfigRegistry::new();
        reg.register(ConfigOption {
            key: "plugins".to_string(),
            r#type: ConfigType::Table,
            default: ConfigValue::Table(BTreeMap::new()),
            env_name: None,
            cli_flag: None,
            merge: MergeStrategy::Merge,
            description: "plugins".to_string(),
        });

        let mut lower = BTreeMap::new();
        lower.insert("a".to_string(), ConfigValue::String("lower-a".to_string()));
        lower.insert("b".to_string(), ConfigValue::String("lower-b".to_string()));
        reg.set_value(Rank::Defaults, "plugins", ConfigValue::Table(lower));

        let mut higher = BTreeMap::new();
        higher.insert("b".to_string(), ConfigValue::String("higher-b".to_string()));
        higher.insert("c".to_string(), ConfigValue::String("higher-c".to_string()));
        reg.set_value(Rank::Cli, "plugins", ConfigValue::Table(higher));

        let resolved = reg.resolve("plugins").expect("should resolve");
        let table = match resolved.value {
            ConfigValue::Table(t) => t,
            other => panic!("expected table, got {other:?}"),
        };
        assert_eq!(
            table.get("a").and_then(|v| v.as_string()),
            Some("lower-a")
        );
        assert_eq!(
            table.get("b").and_then(|v| v.as_string()),
            Some("higher-b")
        );
        assert_eq!(
            table.get("c").and_then(|v| v.as_string()),
            Some("higher-c")
        );
    }

    /// CLI overrides defaults: the highest rank wins.
    #[test]
    fn test_resolve_picks_highest_rank() {
        let reg = defaults();
        // Defaults set model to claude-3-5-sonnet at rank 5.
        let default = reg.resolve("ai.model").expect("should resolve");
        assert_eq!(default.value.as_string(), Some("claude-3-5-sonnet"));
        assert_eq!(default.source.rank, Rank::Defaults);

        let mut reg = reg;
        reg.set_value(Rank::Cli, "ai.model", ConfigValue::String("override".to_string()));
        let resolved = reg.resolve("ai.model").expect("should resolve");
        assert_eq!(resolved.value.as_string(), Some("override"));
        assert_eq!(resolved.source.rank, Rank::Cli);
    }

    /// Rejected values (unknown key / type mismatch) are recorded in tier
    /// diagnostics.
    #[test]
    fn test_tier_diagnostics() {
        let mut reg = defaults();
        // Unknown key.
        reg.set_value(Rank::Project, "no.such.key", ConfigValue::String("x".to_string()));
        // Type mismatch: ai.model is a String, not Bool.
        reg.set_value(Rank::Project, "ai.model", ConfigValue::Bool(true));

        let diags = reg.tier_diagnostics();
        let reasons = diags.get(Rank::Project).expect("project diagnostics");
        assert_eq!(reasons.len(), 2);
        assert!(reasons.iter().any(|r| r.contains("unknown option key")));
        assert!(reasons.iter().any(|r| r.contains("expects type")));
    }

    /// Parse a TOML string and resolve values from it.
    #[test]
    fn test_load_toml() {
        let mut reg = defaults();
        let toml = r#"
[ai]
model = "gpt-4o"
temperature = 0.3
max_tokens = 4096
"#;
        reg.load_toml(Rank::Project, toml).expect("load");

        let model = reg.resolve("ai.model").expect("resolve model");
        assert_eq!(model.value.as_string(), Some("gpt-4o"));
        assert_eq!(model.source.rank, Rank::Project);

        let temp = reg.resolve("ai.temperature").expect("resolve temperature");
        match temp.value {
            ConfigValue::Float(f) => assert!((f - 0.3).abs() < 1e-9),
            other => panic!("expected float, got {other:?}"),
        }

        let max = reg.resolve("ai.max_tokens").expect("resolve max_tokens");
        match max.value {
            ConfigValue::Int(i) => assert_eq!(i, 4096),
            other => panic!("expected int, got {other:?}"),
        }
    }

    /// `resolve_all` returns every resolvable option.
    #[test]
    fn test_resolve_all() {
        let reg = defaults();
        let all = reg.resolve_all();
        // Every declared option has a default, so all should resolve.
        assert!(all.len() >= 20);
        assert!(all.contains_key("ai.model"));
        assert!(all.contains_key("ui.theme"));
    }
}

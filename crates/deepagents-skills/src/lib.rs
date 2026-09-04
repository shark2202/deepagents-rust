//! 8-source discovery, SKILL.md, rust-embed (Q20)
//!
//! This crate implements the skills subsystem for the deepagents-rust workspace.
//!
//! Skills are reusable, prompt-injectable instructions authored as Markdown
//! files (`SKILL.md`) with YAML frontmatter. The original LangChain Deep
//! Agents SDK discovers skills from up to eight distinct sources; this crate
//! ports that discovery pipeline to Rust, along with parsing, indexing, and a
//! manual trust/untrust list.
//!
//! # Discovery sources
//!
//! The eight discovery sources (see `SkillSource` for the canonical labels):
//!
//! 1. Built-in skills embedded into the binary via `rust-embed`
//! 2. `~/.deepagents/skills/`
//! 3. `./.deepagents/skills/`
//! 4. `~/.agents/skills/` (cross-agent shared)
//! 5. Project `skills/` directory
//! 6. Plugin-contributed skills
//! 7. MCP-contributed skills
//! 8. Marketplace-installed skills
//!
//! # Containment allowlist
//!
//! The original `skills.py` only *displays* the `allowed_tools` containment
//! field in the system prompt; it does not enforce it. This Rust port keeps
//! that original behavior: the field is parsed, surfaced, and rendered into
//! prompts but is **not** used to filter tool calls at runtime.
//!
//! See `docs/SPEC.md` §Q20 for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use deepagents_errors::{ConfigError, Error};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The canonical filename for a skill definition.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// The canonical filename for the manual skill trust list.
pub const TRUST_FILE_NAME: &str = "skill_trust.json";

/// Placeholder for future `rust-embed`-embedded built-in skills.
///
/// At present no skills are embedded into the binary. This struct exists so
/// that the discovery pipeline can be wired up for `rust-embed` without
/// changing the public API. When skills are added, they should be served from
/// an `#[derive(rust_embed::RustEmbed)]` folder and exposed through this type.
#[derive(Debug, Clone, Default)]
pub struct BuiltinSkills;

impl BuiltinSkills {
    /// Returns the list of built-in skills.
    ///
    /// v0: returns an empty vector because no skills are embedded yet.
    #[allow(clippy::unused_self)]
    pub fn list(&self) -> Vec<Skill> {
        Vec::new()
    }
}

// ── 1. SkillFrontmatter ───────────────────────────────────────────────────

/// YAML frontmatter extracted from the top of a `SKILL.md` file.
///
/// Fields mirror the Python SDK's skill frontmatter schema. The
/// `containment` field corresponds to the original `allowed_tools` list and,
/// per the original behavior, is only surfaced in prompts (not enforced).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SkillFrontmatter {
    /// Human-readable skill name (also the lookup key).
    #[serde(default)]
    pub name: String,
    /// Short description shown in the rendered skill index.
    #[serde(default)]
    pub description: String,
    /// Trigger phrases; the skill matches when the user input contains one
    /// (substring) or matches one (regex, see `triggers_as_regex`).
    #[serde(default)]
    pub triggers: Vec<String>,
    /// When `true`, `triggers` are interpreted as regular expressions.
    #[serde(default)]
    pub triggers_as_regex: bool,
    /// Containment allowlist (original `allowed_tools`). Surfaced in the
    /// system prompt but **not** enforced at runtime, matching upstream.
    #[serde(default, rename = "allowed_tools")]
    pub containment: Option<Vec<String>>,
}

// ── 2. SkillSource ─────────────────────────────────────────────────────────

/// The origin of a discovered skill, used for diagnostics and prompt labels.
///
/// Variant names are serialized as snake_case for JSON/YAML stability.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// Built-in skills embedded into the binary via `rust-embed`.
    Builtin,
    /// `~/.deepagents/skills/`
    UserHome,
    /// `./.deepagents/skills/`
    ProjectLocal,
    /// `~/.agents/skills/` (cross-agent shared)
    AgentsShared,
    /// Project `skills/` directory
    ProjectSkills,
    /// Plugin-contributed skills
    Plugin,
    /// MCP-contributed skills
    Mcp,
    /// Marketplace-installed skills
    Marketplace,
}

impl SkillSource {
    /// Returns a human-readable label suitable for the system prompt.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Builtin => "built-in",
            Self::UserHome => "~/.deepagents/skills",
            Self::ProjectLocal => "./.deepagents/skills",
            Self::AgentsShared => "~/.agents/skills",
            Self::ProjectSkills => "project skills",
            Self::Plugin => "plugin",
            Self::Mcp => "mcp",
            Self::Marketplace => "marketplace",
        }
    }
}

// ── 3. Skill ───────────────────────────────────────────────────────────────

/// A parsed skill: frontmatter + markdown body + provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Parsed YAML frontmatter.
    pub frontmatter: SkillFrontmatter,
    /// Markdown body (everything after the frontmatter).
    pub content: String,
    /// Where this skill was discovered.
    pub source: SkillSource,
    /// Absolute filesystem path if the skill was loaded from disk, else `None`
    /// (e.g. for built-in embedded skills).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
}

impl Skill {
    /// Returns the skill's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }

    /// Returns the skill's short description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.frontmatter.description
    }

    /// Returns `true` if the user input matches any trigger.
    ///
    /// When `triggers_as_regex` is `true`, each trigger is compiled as a regex
    /// and the input is tested with [`regex::Regex::is_match`]. Otherwise a
    /// case-insensitive substring match is performed against each trigger.
    #[must_use]
    pub fn matches_trigger(&self, input: &str) -> bool {
        if self.frontmatter.triggers_as_regex {
            for trig in &self.frontmatter.triggers {
                if let Ok(re) = regex::Regex::new(trig)
                    && re.is_match(input)
                {
                    return true;
                }
            }
            false
        } else {
            let lowered = input.to_lowercase();
            self.frontmatter
                .triggers
                .iter()
                .any(|t| lowered.contains(&t.to_lowercase()))
        }
    }
}

// ── 4/5. SkillTrustList ────────────────────────────────────────────────────

/// A single trust-list entry recording whether a skill is explicitly trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTrustEntry {
    /// Name of the skill this entry refers to.
    pub skill_name: String,
    /// Whether the skill is trusted (`true`) or explicitly untrusted (`false`).
    pub trusted: bool,
    /// Unix timestamp (seconds) when the entry was recorded.
    pub added_at: i64,
}

/// Manual trust/untrust list for discovered skills.
///
/// Skills not present in the list are treated as **trusted by default**, which
/// matches the original SDK behavior of trusting locally-discovered skills
/// unless explicitly untrusted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillTrustList {
    /// Map keyed by skill name.
    #[serde(default)]
    pub entries: HashMap<String, SkillTrustEntry>,
}

impl SkillTrustList {
    /// Creates an empty trust list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Marks a skill as trusted, recording the current time.
    pub fn trust(&mut self, name: impl Into<String>) {
        let key = name.into();
        self.entries.insert(
            key.clone(),
            SkillTrustEntry {
                skill_name: key,
                trusted: true,
                added_at: now_unix(),
            },
        );
    }

    /// Marks a skill as untrusted, recording the current time.
    pub fn untrust(&mut self, name: impl Into<String>) {
        let key = name.into();
        self.entries.insert(
            key.clone(),
            SkillTrustEntry {
                skill_name: key,
                trusted: false,
                added_at: now_unix(),
            },
        );
    }

    /// Returns `true` if the skill is trusted.
    ///
    /// Skills absent from the list are trusted by default.
    #[must_use]
    pub fn is_trusted(&self, name: &str) -> bool {
        match self.entries.get(name) {
            Some(entry) => entry.trusted,
            None => true,
        }
    }

    /// Deserializes a trust list from a JSON string.
    ///
    /// # Errors
    /// Returns [`Error::Json`] if the input is not valid JSON for a
    /// [`SkillTrustList`].
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let list = serde_json::from_str::<Self>(json)?;
        Ok(list)
    }

    /// Serializes the trust list to a JSON string.
    ///
    /// # Errors
    /// Returns [`Error::Json`] on serialization failure.
    pub fn to_json(&self) -> Result<String, Error> {
        let s = serde_json::to_string_pretty(self)?;
        Ok(s)
    }
}

/// Returns the current time as a unix timestamp (seconds).
///
/// Uses `SystemTime` to avoid a hard dependency on `chrono`. Panics only if
/// the system clock is set before the unix epoch, which is not a realistic
/// runtime condition.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── 6. SkillParser ─────────────────────────────────────────────────────────

/// Parser for `SKILL.md` files: YAML frontmatter + Markdown body.
#[derive(Debug, Clone, Default)]
pub struct SkillParser;

impl SkillParser {
    /// Parses YAML frontmatter from a `SKILL.md`-style string.
    ///
    /// The frontmatter is delimited by `---` lines at the very start of the
    /// file:
    ///
    /// ```text
    /// ---
    /// name: my-skill
    /// description: ...
    /// ---
    /// # Body markdown
    /// ```
    ///
    /// If no leading `---` is present an empty (default) frontmatter is
    /// returned. If a leading `---` is present but the closing `---` cannot be
    /// found, a [`ConfigError::Load`] is returned.
    ///
    /// # Errors
    /// Returns [`Error::Config`] when the frontmatter block is malformed or
    /// contains invalid YAML.
    pub fn parse_skill_md(content: &str) -> Result<SkillFrontmatter, Error> {
        // Trim a leading BOM if present.
        let content = content.strip_prefix('\u{feff}').unwrap_or(content);

        // Frontmatter must start at the very first line.
        if !content.starts_with("---") {
            return Ok(SkillFrontmatter::default());
        }

        // Split on line-boundary `---` markers.
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return Ok(SkillFrontmatter::default());
        }

        // First line must be exactly `---` (after optional whitespace).
        if lines[0].trim() != "---" {
            return Ok(SkillFrontmatter::default());
        }

        // Find the closing `---` on its own line.
        let mut close_idx = None;
        for (i, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" {
                close_idx = Some(i);
                break;
            }
        }

        let close_idx = match close_idx {
            Some(idx) => idx,
            None => {
                return Err(Error::Config(ConfigError::Load(
                    "skill frontmatter missing closing `---` delimiter".to_string(),
                )));
            }
        };

        let yaml_block: String = lines[1..close_idx].join("\n");
        if yaml_block.trim().is_empty() {
            return Ok(SkillFrontmatter::default());
        }

        let fm: SkillFrontmatter = serde_yaml::from_str(&yaml_block).map_err(|e| {
            Error::Config(ConfigError::Load(format!(
                "skill frontmatter YAML parse error: {e}"
            )))
        })?;
        Ok(fm)
    }

    /// Returns the Markdown body (everything after the frontmatter) for a
    /// `SKILL.md`-style string. Returns the entire string when no frontmatter
    /// is present.
    #[must_use]
    pub fn extract_body(content: &str) -> String {
        let content = content.strip_prefix('\u{feff}').unwrap_or(content);
        if !content.starts_with("---") {
            return content.to_string();
        }
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() || lines[0].trim() != "---" {
            return content.to_string();
        }
        for (i, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" {
                return lines.get(i + 1..).map(|s| s.join("\n")).unwrap_or_default();
            }
        }
        content.to_string()
    }

    /// Reads and parses a `SKILL.md` file from disk into a [`Skill`].
    ///
    /// The resulting [`Skill`] has `source` set to [`SkillSource::ProjectLocal`]
    /// (the caller is expected to override `source`/`path` after discovery if
    /// needed) and `path` set to the absolute path of the file.
    ///
    /// # Errors
    /// Returns [`Error::Io`] on filesystem errors and [`Error::Config`] on
    /// frontmatter parse errors.
    pub fn parse_skill_file(path: impl AsRef<Path>) -> Result<Skill, Error> {
        let path_ref = path.as_ref();
        let raw = std::fs::read_to_string(path_ref)?;
        let frontmatter = Self::parse_skill_md(&raw)?;
        let content = Self::extract_body(&raw);
        let path_str = path_ref.to_string_lossy().to_string();
        Ok(Skill {
            frontmatter,
            content,
            source: SkillSource::ProjectLocal,
            path: Some(path_str),
        })
    }
}

// ── 7. SkillDiscovery ──────────────────────────────────────────────────────

/// The discovery pipeline: walks the 8 sources and applies the trust list.
#[derive(Debug, Clone, Default)]
pub struct SkillDiscovery {
    /// Manual trust/untrust list used to filter discovered skills.
    pub trust_list: SkillTrustList,
}

impl SkillDiscovery {
    /// Creates a new discovery instance with an empty trust list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the trust list used by this discovery instance.
    #[must_use]
    pub fn with_trust_list(mut self, trust: SkillTrustList) -> Self {
        self.trust_list = trust;
        self
    }

    /// Recursively discovers skills in `dir`, tagging each with `source`.
    ///
    /// A file is treated as a skill when its name is [`SKILL_FILE_NAME`] and
    /// its frontmatter parses successfully. Parse errors for individual files
    /// are logged via `tracing` and skipped rather than aborting the walk.
    ///
    /// # Errors
    /// Returns [`Error::Io`] only if `dir` cannot be accessed at all (e.g. the
    /// path is not a directory). Missing directories are reported as an empty
    /// result, not an error, so callers can probe optional sources.
    pub fn discover_from_dir(
        &self,
        dir: impl AsRef<Path>,
        source: SkillSource,
    ) -> Result<Vec<Skill>, Error> {
        let dir_ref = dir.as_ref();
        if !dir_ref.exists() {
            return Ok(Vec::new());
        }
        if !dir_ref.is_dir() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(dir_ref).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if file_name != SKILL_FILE_NAME {
                continue;
            }
            match SkillParser::parse_skill_file(path) {
                Ok(mut skill) => {
                    skill.source = source.clone();
                    skill.path = path.to_string_lossy().to_string().into();
                    out.push(skill);
                }
                Err(e) => {
                    tracing::warn!(
                        "skipping skill file {:?}: {e}",
                        path
                    );
                }
            }
        }
        Ok(out)
    }

    /// Returns built-in (embedded) skills.
    ///
    /// v0: returns an empty vec because no skills are embedded yet; the
    /// `rust-embed` machinery is wired through [`BuiltinSkills`] so skills can
    /// be added later without changing this API.
    #[must_use]
    pub fn discover_builtin(&self) -> Vec<Skill> {
        BuiltinSkills.list()
    }

    /// Discovers skills from all eight sources, skipping missing directories.
    ///
    /// Sources 6 (plugin), 7 (MCP), and 8 (marketplace) are not backed by a
    /// directory in v0; they are probed via the same directory hooks so that
    /// contributors can wire them up later. Each discovered skill keeps the
    /// [`SkillSource`] it was found under.
    ///
    /// # Errors
    /// Returns an error only if a *present* directory cannot be read.
    pub fn discover_all(&self) -> Result<Vec<Skill>, Error> {
        let mut out: Vec<Skill> = Vec::new();

        // 1. Built-in (embedded).
        out.extend(self.discover_builtin());

        // 2. ~/.deepagents/skills/
        if let Some(home) = home_dir() {
            out.extend(self.discover_from_dir(
                home.join(".deepagents").join("skills"),
                SkillSource::UserHome,
            )?);
        }

        // 3. ./.deepagents/skills/
        out.extend(self.discover_from_dir(
            PathBuf::from(".deepagents").join("skills"),
            SkillSource::ProjectLocal,
        )?);

        // 4. ~/.agents/skills/ (cross-agent shared)
        if let Some(home) = home_dir() {
            out.extend(self.discover_from_dir(
                home.join(".agents").join("skills"),
                SkillSource::AgentsShared,
            )?);
        }

        // 5. Project `skills/` directory
        out.extend(self.discover_from_dir(
            PathBuf::from("skills"),
            SkillSource::ProjectSkills,
        )?);

        // 6-8. Plugin / MCP / marketplace — no directory backing in v0, so
        // these contribute nothing yet. The discovery surface is in place so
        // future contributors can plug in their own directories.
        Ok(out)
    }

    /// Filters a set of discovered skills through the trust list.
    #[must_use]
    pub fn filter_trusted(&self, skills: Vec<Skill>) -> Vec<Skill> {
        skills
            .into_iter()
            .filter(|s| self.trust_list.is_trusted(s.name()))
            .collect()
    }
}

/// Resolves the user's home directory using `HOME` (POSIX) semantics.
///
/// Returns `None` if `HOME` is unset or empty, matching `std::env::var`
/// failure and the original SDK's graceful skip of the home-based sources.
fn home_dir() -> Option<PathBuf> {
    match std::env::var("HOME") {
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

// ── 8. SkillIndex ──────────────────────────────────────────────────────────

/// An in-memory index over discovered skills, supporting lookup and search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndex {
    /// All skills held by this index.
    pub skills: Vec<Skill>,
}

impl SkillIndex {
    /// Creates a new index from a vector of skills.
    #[must_use]
    pub fn new(skills: Vec<Skill>) -> Self {
        Self { skills }
    }

    /// Finds a skill by exact name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name() == name)
    }

    /// Searches for skills whose triggers or name match `query`.
    ///
    /// A skill is included if `matches_trigger(query)` is `true` or if the
    /// query appears (case-insensitive) in the skill's name.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&Skill> {
        let lowered = query.to_lowercase();
        self.skills
            .iter()
            .filter(|s| s.matches_trigger(query) || s.name().to_lowercase().contains(&lowered))
            .collect()
    }

    /// Returns a slice of all skills.
    #[must_use]
    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    /// Renders a one-skill-per-line summary for system prompt injection.
    ///
    /// Each line is `- <name>: <description>`. Skills with empty names are
    /// skipped. This mirrors the original SDK's skill index block.
    #[must_use]
    pub fn render_index(&self) -> String {
        let mut buf = String::new();
        for s in &self.skills {
            if s.name().is_empty() {
                continue;
            }
            buf.push_str("- ");
            buf.push_str(s.name());
            if !s.description().is_empty() {
                buf.push_str(": ");
                buf.push_str(s.description());
            }
            buf.push('\n');
        }
        buf
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_in_result, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample_skill_md() -> &'static str {
        "\
---
name: weather
description: Look up the weather for a city
triggers:
  - weather
  - forecast
triggers_as_regex: false
allowed_tools:
  - get_weather
---
# Weather skill

Ask the user for a city, then call get_weather.
"
    }

    #[test]
    fn test_skill_frontmatter_serde() {
        let fm = SkillFrontmatter {
            name: "weather".to_string(),
            description: "look up weather".to_string(),
            triggers: vec!["weather".to_string()],
            triggers_as_regex: false,
            containment: Some(vec!["get_weather".to_string()]),
        };
        let yaml = serde_yaml::to_string(&fm).unwrap();
        let back: SkillFrontmatter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(fm, back);

        // JSON round-trip and containment rename check.
        let json = serde_json::json!({
            "name": "x",
            "description": "d",
            "triggers": ["a"],
            "triggers_as_regex": true,
            "allowed_tools": ["t1"],
        });
        let parsed: SkillFrontmatter = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.containment, Some(vec!["t1".to_string()]));
    }

    #[test]
    fn test_parse_skill_md() {
        let fm = SkillParser::parse_skill_md(sample_skill_md()).unwrap();
        assert_eq!(fm.name, "weather");
        assert_eq!(fm.description, "Look up the weather for a city");
        assert_eq!(fm.triggers, vec!["weather", "forecast"]);
        assert!(!fm.triggers_as_regex);
        assert_eq!(fm.containment, Some(vec!["get_weather".to_string()]));
    }

    #[test]
    fn test_parse_skill_md_no_frontmatter() {
        // No frontmatter at all → empty (default) frontmatter, not an error.
        let body = "# just markdown\nno frontmatter here";
        let fm = SkillParser::parse_skill_md(body).unwrap();
        assert!(fm.name.is_empty());
        assert!(fm.triggers.is_empty());

        // Unclosed frontmatter → error.
        let bad = "---\nname: broken\n";
        let err = SkillParser::parse_skill_md(bad).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("frontmatter"), "{s}");
    }

    #[test]
    fn test_skill_matches_trigger_substring() {
        let skill = Skill {
            frontmatter: SkillFrontmatter {
                name: "weather".to_string(),
                description: String::new(),
                triggers: vec!["weather".to_string(), "forecast".to_string()],
                triggers_as_regex: false,
                containment: None,
            },
            content: String::new(),
            source: SkillSource::Builtin,
            path: None,
        };
        assert!(skill.matches_trigger("What is the weather in Tokyo?"));
        assert!(skill.matches_trigger("FORECAST for tomorrow"));
        assert!(!skill.matches_trigger("what time is it"));
    }

    #[test]
    fn test_skill_matches_trigger_regex() {
        let skill = Skill {
            frontmatter: SkillFrontmatter {
                name: "deploy".to_string(),
                description: String::new(),
                triggers: vec!["deploy\\s+(prod|staging)".to_string()],
                triggers_as_regex: true,
                containment: None,
            },
            content: String::new(),
            source: SkillSource::Builtin,
            path: None,
        };
        assert!(skill.matches_trigger("please deploy prod now"));
        assert!(skill.matches_trigger("deploy staging"));
        assert!(!skill.matches_trigger("deploying things"));
    }

    #[test]
    fn test_skill_trust_list() {
        let mut list = SkillTrustList::new();
        // Default: not present → trusted.
        assert!(list.is_trusted("unknown"));

        list.untrust("dangerous");
        assert!(!list.is_trusted("dangerous"));

        list.trust("dangerous");
        assert!(list.is_trusted("dangerous"));

        // Round-trip JSON.
        let json = list.to_json().unwrap();
        let back = SkillTrustList::from_json(&json).unwrap();
        assert!(back.is_trusted("dangerous"));
        assert!(back.is_trusted("still-unknown"));
    }

    #[test]
    fn test_skill_discovery_from_dir() {
        // Use a unique subdir under temp_dir to avoid collisions.
        let mut base = std::env::temp_dir();
        base.push(format!(
            "deepagents-skills-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();

        // Tidy up at the end of the test.
        struct Guard(std::path::PathBuf);
        impl std::ops::Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _guard = Guard(base.clone());

        let skill_dir = base.join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join(SKILL_FILE_NAME), sample_skill_md()).unwrap();

        // Non-skill markdown file should be ignored.
        std::fs::write(skill_dir.join("README.md"), "# readme").unwrap();

        let discovery = SkillDiscovery::new();
        let found = discovery
            .discover_from_dir(&base, SkillSource::ProjectSkills)
            .unwrap();
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!(s.name(), "weather");
        assert_eq!(s.source, SkillSource::ProjectSkills);
        assert!(s.path.as_ref().unwrap().ends_with(SKILL_FILE_NAME));

        // Trust filtering.
        let mut trust = SkillTrustList::new();
        trust.untrust("weather");
        let discovery = SkillDiscovery::new().with_trust_list(trust);
        let filtered = discovery.filter_trusted(found);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_skill_index_render() {
        let skills = vec![
            Skill {
                frontmatter: SkillFrontmatter {
                    name: "weather".to_string(),
                    description: "Get the forecast".to_string(),
                    triggers: vec![],
                    triggers_as_regex: false,
                    containment: None,
                },
                content: String::new(),
                source: SkillSource::Builtin,
                path: None,
            },
            Skill {
                frontmatter: SkillFrontmatter {
                    name: String::new(),
                    description: "no name".to_string(),
                    triggers: vec![],
                    triggers_as_regex: false,
                    containment: None,
                },
                content: String::new(),
                source: SkillSource::Builtin,
                path: None,
            },
        ];
        let idx = SkillIndex::new(skills);
        let rendered = idx.render_index();
        assert_eq!(rendered, "- weather: Get the forecast\n");

        // find + search.
        assert!(idx.find("weather").is_some());
        assert!(idx.find("missing").is_none());
        assert_eq!(idx.search("weather").len(), 1);
    }

    #[test]
    fn test_skill_source_labels_and_serde() {
        // Every variant should serialize to snake_case and have a label.
        let cases = vec![
            (SkillSource::Builtin, "builtin", "built-in"),
            (SkillSource::UserHome, "user_home", "~/.deepagents/skills"),
            (SkillSource::ProjectLocal, "project_local", "./.deepagents/skills"),
            (SkillSource::AgentsShared, "agents_shared", "~/.agents/skills"),
            (SkillSource::ProjectSkills, "project_skills", "project skills"),
            (SkillSource::Plugin, "plugin", "plugin"),
            (SkillSource::Mcp, "mcp", "mcp"),
            (SkillSource::Marketplace, "marketplace", "marketplace"),
        ];
        for (variant, serde_name, label) in cases {
            assert_eq!(variant.label(), label);
            let s = serde_json::to_string(&variant).unwrap();
            assert!(s.contains(serde_name), "{s:?} missing {serde_name}");
            let back: SkillSource = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant);
        }
    }
}

//! Filesystem permission model (Q7).
//!
//! Permissions are enforced at the tool layer, not the backend layer.
//! The [`FilesystemMiddleware`](crate) checks each tool call's path against
//! the configured permission rules before delegating to the backend.
//!
//! - `Allow` → normal execution
//! - `Deny` → tool returns permission-denied error
//! - `Interrupt` → `on_tool_call` returns `ToolCallAction::skip` (v0: treated
//!   as Deny with a distinct message; true pause/resume deferred to v1
//!   `deepagents-sessions`)
//!
//! Glob matching uses the `globset` crate. Paths must start with `/` and must
//! not contain `..` or `~`.

use globset::GlobBuilder;
use serde::{Deserialize, Serialize};

// ── Operation / Mode ───────────────────────────────────────────────────

/// The filesystem operation a permission rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemOperation {
    /// Read operations (ls, read, grep, glob, info).
    Read,
    /// Write operations (write, edit, delete).
    Write,
}

/// The enforcement mode for a permission rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// Allow the operation (no interception).
    Allow,
    /// Deny the operation (return permission-denied error).
    Deny,
    /// Interrupt the run (trigger HITL pause+resume).
    Interrupt,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::Allow
    }
}

// ── FilesystemPermission ───────────────────────────────────────────────

/// A single permission rule: which operations on which path globs get which mode.
///
/// Path patterns are glob patterns that must start with `/` and must not
/// contain `..` or `~`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemPermission {
    /// Operations this rule applies to.
    pub operations: Vec<FilesystemOperation>,
    /// Glob patterns (e.g. `/src/**`, `/tmp/*.log`).
    pub paths: Vec<String>,
    /// Enforcement mode.
    pub mode: PermissionMode,
}

impl FilesystemPermission {
    /// Create a new permission rule.
    pub fn new(
        operations: Vec<FilesystemOperation>,
        paths: Vec<String>,
        mode: PermissionMode,
    ) -> Self {
        Self {
            operations,
            paths,
            mode,
        }
    }

    /// Create an allow rule for the given operations and paths.
    pub fn allow(operations: Vec<FilesystemOperation>, paths: Vec<String>) -> Self {
        Self::new(operations, paths, PermissionMode::Allow)
    }

    /// Create a deny rule for the given operations and paths.
    pub fn deny(operations: Vec<FilesystemOperation>, paths: Vec<String>) -> Self {
        Self::new(operations, paths, PermissionMode::Deny)
    }

    /// Create an interrupt rule for the given operations and paths.
    pub fn interrupt(operations: Vec<FilesystemOperation>, paths: Vec<String>) -> Self {
        Self::new(operations, paths, PermissionMode::Interrupt)
    }

    /// Check if this rule applies to the given operation and path.
    ///
    /// A rule applies when:
    /// 1. The operation is in `operations`, AND
    /// 2. The path matches at least one glob pattern.
    pub fn matches(&self, op: FilesystemOperation, path: &str) -> bool {
        if !self.operations.contains(&op) {
            return false;
        }
        for pattern in &self.paths {
            if let Ok(glob) = GlobBuilder::new(pattern).build() {
                if glob.compile_matcher().is_match(path) {
                    return true;
                }
            }
        }
        false
    }

    /// Validate that all paths in this rule are well-formed.
    ///
    /// Paths must start with `/` and must not contain `..` or `~`.
    pub fn validate(&self) -> Result<(), String> {
        for path in &self.paths {
            if !path.starts_with('/') {
                return Err(format!("path must start with /: {path}"));
            }
            if path.contains("..") {
                return Err(format!("path must not contain ..: {path}"));
            }
            if path.contains('~') {
                return Err(format!("path must not contain ~: {path}"));
            }
        }
        Ok(())
    }
}

// ── PermissionChecker ──────────────────────────────────────────────────

/// Checks tool calls against a set of permission rules.
///
/// Rules are evaluated in order; the first matching rule's mode wins.
/// If no rule matches, the default is `Allow`.
#[derive(Debug, Clone, Default)]
pub struct PermissionChecker {
    /// Ordered permission rules.
    rules: Vec<FilesystemPermission>,
}

impl PermissionChecker {
    /// Create a new permission checker with the given rules.
    pub fn new(rules: Vec<FilesystemPermission>) -> Self {
        Self { rules }
    }

    /// Add a rule to the checker.
    pub fn add_rule(&mut self, rule: FilesystemPermission) {
        self.rules.push(rule);
    }

    /// Check an operation + path against all rules.
    ///
    /// Returns the mode of the first matching rule, or `Allow` if none match.
    pub fn check(&self, op: FilesystemOperation, path: &str) -> PermissionMode {
        for rule in &self.rules {
            if rule.matches(op, path) {
                return rule.mode;
            }
        }
        PermissionMode::Allow
    }

    /// Check if any rule has `Interrupt` mode for the given operation.
    ///
    /// In v0, this method is provided as a public API but is not called
    /// internally — HITL hook registration is driven by `interrupt_on` being
    /// `Some(...)`, not by querying permission rules. The builder's
    /// `interrupt_on` map is populated from `with_interrupt_on()` and also
    /// auto-derived from permission rules via [`to_interrupt_map`](Self::to_interrupt_map).
    ///
    /// In v1, the runner will call this to decide whether to register
    /// a checkpoint/pause hook for agents that have interrupt rules but
    /// no explicit `interrupt_on` map.
    pub fn has_interrupt_rules(&self) -> bool {
        self.rules
            .iter()
            .any(|r| r.mode == PermissionMode::Interrupt)
    }

    /// Collect all interrupt rules and convert them to interrupt policies.
    ///
    /// Each interrupt permission rule becomes an entry in the `interrupt_on`
    /// map: the glob pattern is the key, and the policy is `Simple(false)`
    /// (i.e. require human approval).
    pub fn to_interrupt_policies(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| r.mode == PermissionMode::Interrupt)
            .flat_map(|r| r.paths.clone())
            .collect()
    }

    /// Get all rules.
    pub fn rules(&self) -> &[FilesystemPermission] {
        &self.rules
    }

    /// Validate all rules.
    pub fn validate_all(&self) -> Result<(), String> {
        for rule in &self.rules {
            rule.validate()?;
        }
        Ok(())
    }
}

impl PermissionChecker {
    /// Create an empty permission checker (everything allowed).
    pub fn empty() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allow_by_default() {
        let checker = PermissionChecker::empty();
        assert_eq!(
            checker.check(FilesystemOperation::Read, "/any/path"),
            PermissionMode::Allow
        );
    }

    #[test]
    fn test_deny_rule() {
        let checker = PermissionChecker::new(vec![FilesystemPermission::deny(
            vec![FilesystemOperation::Write],
            vec!["/etc/**".to_string()],
        )]);
        assert_eq!(
            checker.check(FilesystemOperation::Write, "/etc/passwd"),
            PermissionMode::Deny
        );
        assert_eq!(
            checker.check(FilesystemOperation::Read, "/etc/passwd"),
            PermissionMode::Allow
        );
        assert_eq!(
            checker.check(FilesystemOperation::Write, "/home/user/file"),
            PermissionMode::Allow
        );
    }

    #[test]
    fn test_interrupt_rule() {
        let checker = PermissionChecker::new(vec![FilesystemPermission::interrupt(
            vec![FilesystemOperation::Write],
            vec!["/prod/**".to_string()],
        )]);
        assert_eq!(
            checker.check(FilesystemOperation::Write, "/prod/app/config"),
            PermissionMode::Interrupt
        );
        assert!(checker.has_interrupt_rules());
        assert_eq!(
            checker.to_interrupt_policies(),
            vec!["/prod/**".to_string()]
        );
    }

    #[test]
    fn test_glob_matching() {
        let checker = PermissionChecker::new(vec![FilesystemPermission::deny(
            vec![FilesystemOperation::Read],
            vec!["/src/**/*.rs".to_string()],
        )]);
        assert_eq!(
            checker.check(FilesystemOperation::Read, "/src/main.rs"),
            PermissionMode::Deny
        );
        assert_eq!(
            checker.check(FilesystemOperation::Read, "/src/deep/mod.rs"),
            PermissionMode::Deny
        );
        assert_eq!(
            checker.check(FilesystemOperation::Read, "/src/README.md"),
            PermissionMode::Allow
        );
    }

    #[test]
    fn test_first_match_wins() {
        let checker = PermissionChecker::new(vec![
            FilesystemPermission::allow(
                vec![FilesystemOperation::Write],
                vec!["/tmp/**".to_string()],
            ),
            FilesystemPermission::deny(
                vec![FilesystemOperation::Write],
                vec!["/tmp/secret/**".to_string()],
            ),
        ]);
        // Allow rule comes first and matches → Allow wins
        assert_eq!(
            checker.check(FilesystemOperation::Write, "/tmp/secret/file"),
            PermissionMode::Allow
        );
    }

    #[test]
    fn test_path_validation() {
        let rule = FilesystemPermission::deny(
            vec![FilesystemOperation::Write],
            vec!["relative/path".to_string()],
        );
        assert!(rule.validate().is_err());

        let rule = FilesystemPermission::deny(
            vec![FilesystemOperation::Write],
            vec!["/etc/../passwd".to_string()],
        );
        assert!(rule.validate().is_err());

        let rule = FilesystemPermission::deny(
            vec![FilesystemOperation::Write],
            vec!["/etc/passwd".to_string()],
        );
        assert!(rule.validate().is_ok());
    }
}

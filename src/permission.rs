//! `FilesystemPermission` + `check_fs_permission` —— 对应 deepagents `permissions.py` + `_check_fs_permission`。
//!
//! first-match-wins over ordered rules。mode: `Allow`/`Deny`/`Interrupt`。
//! **工具层只 enforce `Deny`**（pre-check + post-filter）；`Interrupt` 交 HITL 中间件
//! （defer 到 Phase 2b），工具层视为 `Allow`——对齐 deepagents spec（`_fs_interrupt.py`：
//! FilesystemMiddleware 只管 deny，interrupt 由 HumanInTheLoopMiddleware + when 谓词驱动）。
//!
//! glob 匹配用 `globset`（支持 `**`/`?`/`[abc]`，等价 wcmatch GLOBSTAR；BRACE `{a,b}` defer）。

use globset::Glob;
use std::fmt;

/// 文件操作分类。`execute` 不在此（无 permission 分类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilesystemOperation {
    Read,
    Write,
}

impl fmt::Display for FilesystemOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
        }
    }
}

/// 权限模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    /// 允许（默认）。
    Allow,
    /// 拒绝（工具层 enforce）。
    Deny,
    /// 中断（交 HITL，工具层视为 allow）。
    Interrupt,
}

/// 单条权限规则。`paths` 是 glob patterns，必须以 `/` 开头、无 `..`、无 `~`。
#[derive(Clone, Debug)]
pub struct FilesystemPermission {
    /// 该规则适用的操作（read/write）。
    pub operations: Vec<FilesystemOperation>,
    /// glob patterns（如 `/secret/**`）。
    pub paths: Vec<String>,
    /// 模式。
    pub mode: PermissionMode,
}

impl FilesystemPermission {
    /// 构造（校验 paths）。
    ///
    /// # Errors
    /// path 不以 `/` 开头、含 `..` 或 `~`。
    pub fn new(
        operations: Vec<FilesystemOperation>,
        paths: Vec<String>,
        mode: PermissionMode,
    ) -> Result<Self, String> {
        for p in &paths {
            if !p.starts_with('/') {
                return Err(format!("path must start with '/': {p}"));
            }
            if p.split('/').any(|c| c == "..") {
                return Err(format!("path must not contain '..': {p}"));
            }
            if p.contains('~') {
                return Err(format!("path must not contain '~': {p}"));
            }
        }
        Ok(Self { operations, paths, mode })
    }

    /// `deny` 规则快捷构造。
    /// # Errors
    /// 见 [`Self::new`]。
    pub fn deny(operations: Vec<FilesystemOperation>, paths: Vec<String>) -> Result<Self, String> {
        Self::new(operations, paths, PermissionMode::Deny)
    }

    /// `allow` 规则快捷构造（显式允许，用于覆盖前序 deny）。
    /// # Errors
    /// 见 [`Self::new`]。
    pub fn allow(operations: Vec<FilesystemOperation>, paths: Vec<String>) -> Result<Self, String> {
        Self::new(operations, paths, PermissionMode::Allow)
    }

    /// `interrupt` 规则快捷构造（HITL defer）。
    /// # Errors
    /// 见 [`Self::new`]。
    pub fn interrupt(operations: Vec<FilesystemOperation>, paths: Vec<String>) -> Result<Self, String> {
        Self::new(operations, paths, PermissionMode::Interrupt)
    }
}

/// first-match-wins。无匹配 → `Allow`。
///
/// glob 匹配 path（绝对 或 去前导 `/` 的相对，兼容 backend virtual path）。
#[must_use]
pub fn check_fs_permission(
    rules: &[FilesystemPermission],
    op: FilesystemOperation,
    path: &str,
) -> PermissionMode {
    for rule in rules {
        if !rule.operations.contains(&op) {
            continue;
        }
        for pattern in &rule.paths {
            if let Ok(g) = Glob::new(pattern) {
                let matcher = g.compile_matcher();
                if matcher.is_match(path) || matcher.is_match(path.trim_start_matches('/')) {
                    return rule.mode;
                }
            }
        }
    }
    PermissionMode::Allow
}

/// 工具层 deny 检查：`Deny` → `Err`，`Allow`/`Interrupt` → `Ok`（interrupt 交 HITL defer）。
///
/// 返回的 Err 字符串形如 `permission denied for write on /secret/x`，
/// 工具层包装成 `Error: ...` 返回（对齐 deepagents ToolMessage status=error）。
pub fn check_deny(
    rules: &[FilesystemPermission],
    op: FilesystemOperation,
    path: &str,
) -> Result<(), String> {
    match check_fs_permission(rules, op, path) {
        PermissionMode::Deny => Err(format!("permission denied for {op} on {path}")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_wins_first_match() {
        let rules = vec![
            FilesystemPermission::deny(vec![FilesystemOperation::Write], vec!["/secret/**".into()]).unwrap(),
            FilesystemPermission::allow(vec![FilesystemOperation::Write], vec!["/**".into()]).unwrap(),
        ];
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Write, "/secret/x"), PermissionMode::Deny);
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Write, "/public/x"), PermissionMode::Allow);
    }

    #[test]
    fn operation_filter() {
        let rules = vec![
            FilesystemPermission::deny(vec![FilesystemOperation::Write], vec!["/ro/**".into()]).unwrap(),
        ];
        // write deny, read allow
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Write, "/ro/x"), PermissionMode::Deny);
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Read, "/ro/x"), PermissionMode::Allow);
    }

    #[test]
    fn interrupt_treated_as_allow_at_tool_level() {
        let rules = vec![
            FilesystemPermission::interrupt(vec![FilesystemOperation::Write], vec!["/approve/**".into()]).unwrap(),
        ];
        // 工具层 check_deny 视为 allow（HITL defer）
        assert!(check_deny(&rules, FilesystemOperation::Write, "/approve/x").is_ok());
    }

    #[test]
    fn path_validation_rejects_dotdot() {
        assert!(FilesystemPermission::deny(vec![FilesystemOperation::Write], vec!["/../x".into()]).is_err());
        assert!(FilesystemPermission::deny(vec![FilesystemOperation::Write], vec!["relative/path".into()]).is_err());
    }

    #[test]
    fn globstar_matches_recursively() {
        let rules = vec![
            FilesystemPermission::deny(vec![FilesystemOperation::Read], vec!["/secret/**/*.env".into()]).unwrap(),
        ];
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Read, "/secret/sub/.env"), PermissionMode::Deny);
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Read, "/secret/.env"), PermissionMode::Deny);
        assert_eq!(check_fs_permission(&rules, FilesystemOperation::Read, "/secret/x.txt"), PermissionMode::Allow);
    }
}

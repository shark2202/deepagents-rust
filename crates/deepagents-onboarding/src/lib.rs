//! marker files, name memory (Q25)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.
//!
//! This crate manages the onboarding lifecycle state for the Deep Agents CLI.
//! Onboarding is a short, interactive flow presented to first-run users:
//!
//! 1. **Goal criteria preference** — whether to enable goal criteria.
//! 2. **Name screen** — ask the user for their name.
//! 3. **Dependencies screen** — install runtime dependencies.
//! 4. **Name memory** — write the user's name into AI session memory, then
//!    mark onboarding as complete.
//!
//! This crate owns only the *state* side of that flow: it reads and writes
//! marker files under `~/.deepagents/.state/` and provides helpers to wrap /
//! unwrap the user's name inside the AI session memory block. The TUI layer
//! (the `deepagents-tui` crate) drives the screens themselves.
//!
//! ## Marker files
//!
//! Two marker files live in the `.state` directory:
//!
//! | File | Meaning |
//! | --- | --- |
//! | `onboarding_complete` | Onboarding has finished. |
//! | `goal_auto_accept_prompt_shown` | The goal auto-accept prompt was shown. |
//!
//! Presence of a marker file is the source of truth for the corresponding
//! boolean in [`OnboardingState`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use deepagents_errors::Error;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Name memory block markers ──────────────────────────────────────────

/// Opening tag of the onboarding-name memory block.
///
/// Wraps the user's name inside an AI session memory block so that downstream
/// memory tooling can locate and extract it without parsing free-form prose.
pub const ONBOARDING_NAME_OPEN: &str = "<onboarding_name>";

/// Closing tag of the onboarding-name memory block.
///
/// Companion to [`ONBOARDING_NAME_OPEN`]; the two tags sandwich the raw name.
pub const ONBOARDING_NAME_CLOSE: &str = "</onboarding_name>";

// ── OnboardingMarker ───────────────────────────────────────────────────

/// A marker file that records a discrete onboarding milestone.
///
/// Each variant maps to a filename under the `.state` directory. The presence
/// of the file is the source of truth for the onboarding state derived in
/// [`OnboardingManager::load_state`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum OnboardingMarker {
    /// Onboarding has been completed end-to-end.
    ///
    /// File: `onboarding_complete`.
    OnboardingComplete,
    /// The goal auto-accept prompt has been shown to the user.
    ///
    /// File: `goal_auto_accept_prompt_shown`.
    GoalAutoAcceptPromptShown,
}

impl OnboardingMarker {
    /// Returns the marker's on-disk filename (relative to the state dir).
    ///
    /// The returned string is a bare filename — no path separators — suitable
    /// for joining onto the `.state` directory path.
    pub fn filename(&self) -> &str {
        match self {
            OnboardingMarker::OnboardingComplete => "onboarding_complete",
            OnboardingMarker::GoalAutoAcceptPromptShown => "goal_auto_accept_prompt_shown",
        }
    }
}

// ── OnboardingState ────────────────────────────────────────────────────

/// A point-in-time snapshot of the onboarding lifecycle state.
///
/// Built by [`OnboardingManager::load_state`] from the set of marker files
/// currently on disk. Fields mirror the marker files plus the in-memory-only
/// `name` value, which is not persisted by this crate (it is carried in the
/// AI session memory block instead, see [`write_onboarding_name_memory`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingState {
    /// Whether `onboarding_complete` is present.
    completed: bool,
    /// Whether `goal_auto_accept_prompt_shown` is present.
    goal_prompt_shown: bool,
    /// The user's name, if captured during onboarding (not persisted to disk
    /// by this crate; populated by callers after extracting the memory block).
    name: Option<String>,
    /// Whether the user opted into goal criteria during onboarding.
    goal_criteria_enabled: bool,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self::new()
    }
}

impl OnboardingState {
    /// Creates a fresh, fully-unstarted onboarding state: nothing completed,
    /// no prompts shown, no name, goal criteria disabled.
    pub fn new() -> Self {
        OnboardingState {
            completed: false,
            goal_prompt_shown: false,
            name: None,
            goal_criteria_enabled: false,
        }
    }

    /// Returns `true` if onboarding has been completed
    /// (`onboarding_complete` marker present).
    pub fn is_complete(&self) -> bool {
        self.completed
    }
}

// ── OnboardingManager ──────────────────────────────────────────────────

/// Reads and writes the onboarding marker files under a `.state` directory.
///
/// The manager is intentionally stateless beyond its configured directory:
/// every read/write goes straight to disk so that concurrent or restartable
/// flows always observe the latest on-disk truth.
#[derive(Debug, Clone)]
pub struct OnboardingManager {
    /// Absolute path to the `.state` directory that holds the marker files
    /// (e.g. `~/.deepagents/.state/`).
    state_dir: PathBuf,
}

impl OnboardingManager {
    /// Creates a new manager rooted at the given `.state` directory.
    ///
    /// The directory need not exist yet; callers that intend to write markers
    /// should invoke [`Self::ensure_state_dir`] first.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        OnboardingManager {
            state_dir: state_dir.into(),
        }
    }

    /// Returns the default `.state` directory: `~/.deepagents/.state/`.
    ///
    /// The home directory is resolved from the `HOME` environment variable.
    /// When `HOME` is unset or empty (e.g. in minimal CI containers), this
    /// falls back to `/tmp` so that the path is still usable and absolute.
    pub fn default_state_dir() -> PathBuf {
        let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
        let base = match home {
            Some(h) => PathBuf::from(h),
            None => PathBuf::from("/tmp"),
        };
        base.join(".deepagents").join(".state")
    }

    /// Returns a reference to the configured state directory.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Creates the state directory (and parents) if it does not already exist.
    ///
    /// No-op if the directory already exists. Returns an
    /// [`deepagents_errors::Error`] wrapping the underlying IO error if the
    /// directory cannot be created.
    pub fn ensure_state_dir(&self) -> Result<(), Error> {
        if self.state_dir.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.state_dir)?;
        Ok(())
    }

    /// Resolves the on-disk path of a marker file.
    fn marker_path(&self, marker: &OnboardingMarker) -> PathBuf {
        self.state_dir.join(marker.filename())
    }

    /// Returns `true` if the marker file currently exists on disk.
    pub fn is_marker_present(&self, marker: OnboardingMarker) -> bool {
        self.marker_path(&marker).exists()
    }

    /// Creates the marker file (a zero-byte "touch").
    ///
    /// Ensures the parent state directory exists first, then creates the file
    /// if it is not already present. Idempotent: calling this twice is safe.
    pub fn set_marker(&self, marker: OnboardingMarker) -> Result<(), Error> {
        self.ensure_state_dir()?;
        let path = self.marker_path(&marker);
        if path.exists() {
            return Ok(());
        }
        // Create (or truncate-to-empty) the marker file.
        std::fs::File::create(&path)?;
        Ok(())
    }

    /// Removes the marker file if it exists.
    ///
    /// Idempotent: clearing a marker that is not present is a no-op.
    pub fn clear_marker(&self, marker: OnboardingMarker) -> Result<(), Error> {
        let path = self.marker_path(&marker);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Inspects the marker files on disk and builds the current
    /// [`OnboardingState`].
    ///
    /// `name` and `goal_criteria_enabled` are not persisted by this crate, so
    /// they are left as `None` / `false` for callers to fill in from session
    /// memory.
    pub fn load_state(&self) -> OnboardingState {
        OnboardingState {
            completed: self.is_marker_present(OnboardingMarker::OnboardingComplete),
            goal_prompt_shown: self.is_marker_present(
                OnboardingMarker::GoalAutoAcceptPromptShown,
            ),
            name: None,
            goal_criteria_enabled: false,
        }
    }

    /// Writes the `onboarding_complete` marker, marking onboarding as done.
    pub fn mark_onboarding_complete(&self) -> Result<(), Error> {
        self.set_marker(OnboardingMarker::OnboardingComplete)
    }

    /// Writes the `goal_auto_accept_prompt_shown` marker.
    pub fn mark_goal_prompt_shown(&self) -> Result<(), Error> {
        self.set_marker(OnboardingMarker::GoalAutoAcceptPromptShown)
    }
}

// ── Name memory helpers ────────────────────────────────────────────────

/// Wraps a user name in the onboarding-name memory block.
///
/// Returns a string of the form:
///
/// ```text
/// <onboarding_name>
/// {name}
/// </onboarding_name>
/// ```
///
/// The wrapping tags let downstream memory tooling extract the name without
/// parsing prose. The `name` is inserted verbatim; callers should trim or
/// validate it beforehand.
pub fn write_onboarding_name_memory(name: &str) -> String {
    format!("{ONBOARDING_NAME_OPEN}\n{name}\n{ONBOARDING_NAME_CLOSE}")
}

/// Extracts the user name from an onboarding-name memory block, if present.
///
/// Scans `text` for an
/// [`ONBOARDING_NAME_OPEN`] … [`ONBOARDING_NAME_CLOSE`] pair and returns the
/// trimmed content between them. Returns `None` if no well-formed block is
/// found. When multiple blocks exist, the first match wins.
pub fn extract_onboarding_name_block(text: &str) -> Option<String> {
    let start = text.find(ONBOARDING_NAME_OPEN)?;
    let after_open = start + ONBOARDING_NAME_OPEN.len();
    let end = text[after_open..].find(ONBOARDING_NAME_CLOSE)?;
    let inner = &text[after_open..after_open + end];
    let trimmed = inner.trim_matches('\n').trim();
    Some(trimmed.to_string())
}

/// Removes the onboarding-name memory-block tags from `text`.
///
/// Any `<onboarding_name>` / `</onboarding_name>` tags are deleted, leaving
/// their inner content (and surrounding text) in place. This is useful when
/// rendering memory to the user or when feeding a memory buffer to a model
/// that should not see the raw structural tags.
pub fn strip_onboarding_name_markers(text: &str) -> String {
    text.replace(ONBOARDING_NAME_OPEN, "")
        .replace(ONBOARDING_NAME_CLOSE, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Generates a unique subdirectory under the system temp dir for an
    /// isolated test run. A monotonic counter plus PID keeps tests parallel-safe.
    fn unique_state_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "deepagents-onboarding-test-{}-{n}",
            std::process::id()
        ))
    }

    #[test]
    fn test_onboarding_marker_filename() {
        assert_eq!(
            OnboardingMarker::OnboardingComplete.filename(),
            "onboarding_complete"
        );
        assert_eq!(
            OnboardingMarker::GoalAutoAcceptPromptShown.filename(),
            "goal_auto_accept_prompt_shown"
        );
    }

    #[test]
    fn test_onboarding_state_new() {
        let state = OnboardingState::new();
        assert!(!state.is_complete());
        assert!(!state.goal_prompt_shown);
        assert!(state.name.is_none());
        assert!(!state.goal_criteria_enabled);
    }

    #[test]
    fn test_name_memory_roundtrip() {
        let name = "Ada Lovelace";
        let block = write_onboarding_name_memory(name);
        assert!(block.contains(ONBOARDING_NAME_OPEN));
        assert!(block.contains(ONBOARDING_NAME_CLOSE));
        let extracted = extract_onboarding_name_block(&block);
        assert_eq!(extracted.as_deref(), Some(name));
    }

    #[test]
    fn test_name_memory_extract_preserves_whitespace_names() {
        // Names with internal spaces are preserved verbatim.
        let block = write_onboarding_name_memory("Grace Hopper");
        assert_eq!(
            extract_onboarding_name_block(&block).as_deref(),
            Some("Grace Hopper")
        );
    }

    #[test]
    fn test_name_memory_strip() {
        let name = "Alan Turing";
        let block = write_onboarding_name_memory(name);
        let stripped = strip_onboarding_name_markers(&block);
        assert!(!stripped.contains(ONBOARDING_NAME_OPEN));
        assert!(!stripped.contains(ONBOARDING_NAME_CLOSE));
        // The name content must survive stripping.
        assert!(stripped.contains(name));
    }

    #[test]
    fn test_name_memory_extract_missing_returns_none() {
        assert_eq!(extract_onboarding_name_block("no markers here"), None);
        assert_eq!(
            extract_onboarding_name_block("<onboarding_name>no closer"),
            None
        );
    }

    #[test]
    fn test_onboarding_manager_markers() {
        let dir = unique_state_dir();
        let mgr = OnboardingManager::new(&dir);

        // Initially neither marker is present.
        assert!(!mgr.is_marker_present(OnboardingMarker::OnboardingComplete));
        assert!(!mgr.is_marker_present(
            OnboardingMarker::GoalAutoAcceptPromptShown
        ));

        // Set onboarding_complete and observe it in state.
        mgr.mark_onboarding_complete()
            .expect("mark onboarding complete");
        assert!(mgr.is_marker_present(OnboardingMarker::OnboardingComplete));
        let state = mgr.load_state();
        assert!(state.is_complete());

        // Set the goal-prompt marker too.
        mgr.mark_goal_prompt_shown()
            .expect("mark goal prompt shown");
        assert!(mgr.is_marker_present(
            OnboardingMarker::GoalAutoAcceptPromptShown
        ));
        let state = mgr.load_state();
        assert!(state.is_complete());
        assert!(state.goal_prompt_shown);

        // Clear onboarding_complete; state should reflect it.
        mgr.clear_marker(OnboardingMarker::OnboardingComplete)
            .expect("clear onboarding complete");
        assert!(!mgr.is_marker_present(OnboardingMarker::OnboardingComplete));
        assert!(mgr.is_marker_present(
            OnboardingMarker::GoalAutoAcceptPromptShown
        ));

        // Clearing an already-absent marker is a no-op (idempotent).
        mgr.clear_marker(OnboardingMarker::OnboardingComplete)
            .expect("clear absent marker is a no-op");

        // Setting an existing marker is idempotent.
        mgr.mark_goal_prompt_shown()
            .expect("re-mark goal prompt shown");

        // Cleanup.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ensure_state_dir() {
        // Build a path several levels deep that does not exist yet.
        let base = unique_state_dir();
        let nested = base.join("a").join("b").join("c");
        let mgr = OnboardingManager::new(&nested);

        assert!(!nested.exists());
        mgr.ensure_state_dir().expect("ensure nested state dir");
        assert!(nested.exists());
        assert!(nested.is_dir());

        // Idempotent: calling again must not error.
        mgr.ensure_state_dir().expect("ensure is idempotent");

        // A marker can now be written into the freshly-created dir.
        mgr.mark_onboarding_complete()
            .expect("write marker into nested dir");
        assert!(mgr.is_marker_present(OnboardingMarker::OnboardingComplete));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_default_state_dir_is_under_deepagents() {
        let dir = OnboardingManager::default_state_dir();
        let s = dir.to_string_lossy().to_string();
        assert!(s.ends_with(".deepagents/.state") || s.ends_with(".deepagents/.state/"));
    }
}

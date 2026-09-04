//! Backend trait and result types (Q7).
//!
//! The [`Backend`] trait is the filesystem abstraction used by the
//! FilesystemMiddleware: a virtual POSIX path layer that is platform-agnostic
//! until it hits a concrete disk-backed implementation.
//!
//! v0 provides four implementations:
//! - [`StateBackend`] — in-memory `HashMap`, ephemeral (default)
//! - [`FilesystemBackend`] — `std::fs`, sandboxed to a root dir (feature `filesystem`)
//! - [`LocalShellBackend`] — disk + `std::process::Command` (feature `filesystem`)
//! - [`CompositeBackend`] — longest-prefix routing (feature `composite`)
//!
//! See `docs/SPEC.md` §Q7 for the design rationale.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use deepagents_errors::Error;

// ── Result types ────────────────────────────────────────────────────────

/// A single directory entry returned by [`Backend::ls`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    /// Entry name (basename, no path).
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes (0 for directories).
    pub size: u64,
}

/// Result of [`Backend::ls`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsResult {
    /// The entries in the directory.
    pub entries: Vec<DirEntry>,
}

/// Result of [`Backend::read`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResult {
    /// The file content (UTF-8 text).
    pub content: String,
    /// Total lines in the file.
    pub total_lines: usize,
    /// Whether the read was truncated by `limit`.
    pub truncated: bool,
}

/// A single grep match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    /// 1-based line number.
    pub line_number: usize,
    /// The matching line content.
    pub line: String,
}

/// Result of [`Backend::grep`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepResult {
    /// Matching lines.
    pub matches: Vec<GrepMatch>,
    /// Whether the result was truncated by `max_count`.
    pub truncated: bool,
}

/// Result of [`Backend::glob`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobResult {
    /// Matching file paths (POSIX, `/`-separated).
    pub paths: Vec<String>,
}

/// Result of [`Backend::write`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WriteResult {
    /// Bytes written.
    pub bytes_written: usize,
}

/// Result of [`Backend::edit`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EditResult {
    /// Number of replacements made.
    pub replacements: usize,
}

/// Result of [`Backend::delete`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeleteResult {
    /// Whether a file was actually deleted (false if it didn't exist).
    pub deleted: bool,
}

/// File metadata returned by [`Backend::info`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// POSIX path.
    pub path: String,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// File size in bytes.
    pub size: u64,
}

/// Result of [`Backend::execute`] (sandbox backends only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResponse {
    /// stdout output.
    pub stdout: String,
    /// stderr output.
    pub stderr: String,
    /// Exit code.
    pub exit_code: i32,
    /// Whether the command timed out.
    pub timed_out: bool,
}

// ── Backend trait ───────────────────────────────────────────────────────

/// The filesystem abstraction used by the FilesystemMiddleware.
///
/// All paths are virtual POSIX paths (`/`-separated, absolute, no `..` or `~`).
/// Implementations translate to real paths only at the disk boundary.
#[async_trait]
pub trait Backend: Send + Sync {
    /// List directory entries at `path`.
    async fn ls(&self, path: &str) -> Result<LsResult, Error>;

    /// Read a file, optionally starting at `offset` and limiting to `limit` lines.
    async fn read(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ReadResult, Error>;

    /// Search file contents with a regex `pattern`.
    async fn grep(
        &self,
        pattern: &str,
        path: Option<&str>,
        glob: Option<&str>,
        max_count: Option<usize>,
    ) -> Result<GrepResult, Error>;

    /// Glob-expand a pattern to matching file paths.
    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult, Error>;

    /// Write content to a file (create or overwrite).
    async fn write(&self, path: &str, content: &str) -> Result<WriteResult, Error>;

    /// Replace the first occurrence of `old` with `new` in a file.
    async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult, Error>;

    /// Delete a file.
    async fn delete(&self, path: &str) -> Result<DeleteResult, Error>;

    /// Get file info (metadata).
    async fn info(&self, path: &str) -> Result<FileInfo, Error>;

    /// List all files (for state inspection / testing).
    async fn list_files(&self) -> Result<Vec<FileInfo>, Error>;
}

/// A sandbox backend extends [`Backend`] with command execution.
#[async_trait]
pub trait SandboxBackend: Backend {
    /// Execute a shell command, optionally with a timeout in seconds.
    async fn execute(
        &self,
        command: &str,
        timeout: Option<u64>,
    ) -> Result<ExecuteResponse, Error>;

    /// Stable identifier for this sandbox provider.
    fn id(&self) -> &str;
}

// ── StateBackend (in-memory, ephemeral, default) ───────────────────────

/// In-memory file store: `HashMap<String, String>`.
///
/// This is the default backend — ephemeral, no disk access, safe in all
/// environments including WASM. Used for testing and single-turn agents.
#[derive(Debug, Clone, Default)]
pub struct StateBackend {
    files: Arc<std::sync::Mutex<HashMap<String, String>>>,
}

impl StateBackend {
    /// Create a new empty in-memory backend.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Backend for StateBackend {
    async fn ls(&self, path: &str) -> Result<LsResult, Error> {
        let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let prefix = path.trim_end_matches('/');
        let mut entries: Vec<DirEntry> = Vec::new();
        let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

        for stored_path in files.keys() {
            // Direct file in this directory
            if let Some(rest) = stored_path.strip_prefix(&format!("{prefix}/")) {
                if !rest.contains('/') {
                    entries.push(DirEntry {
                        name: rest.to_string(),
                        is_dir: false,
                        size: files[stored_path].len() as u64,
                    });
                } else if let Some(dir_name) = rest.split('/').next() {
                    if seen_dirs.insert(dir_name.to_string()) {
                        entries.push(DirEntry {
                            name: dir_name.to_string(),
                            is_dir: true,
                            size: 0,
                        });
                    }
                }
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(LsResult { entries })
    }

    async fn read(
        &self,
        path: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ReadResult, Error> {
        let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let content = files
            .get(path)
            .ok_or_else(|| Error::Sandbox(deepagents_errors::SandboxError::NotFound(path.into())))?
            .clone();
        drop(files);

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let start = offset.min(total_lines);
        let end = if limit == 0 {
            total_lines
        } else {
            (start + limit).min(total_lines)
        };
        let truncated = end < total_lines;
        let content = lines[start..end].join("\n");
        Ok(ReadResult {
            content,
            total_lines,
            truncated,
        })
    }

    async fn grep(
        &self,
        pattern: &str,
        path: Option<&str>,
        _glob: Option<&str>,
        max_count: Option<usize>,
    ) -> Result<GrepResult, Error> {
        let re = regex::Regex::new(pattern).map_err(|e| {
            Error::Sandbox(deepagents_errors::SandboxError::Provider(format!(
                "invalid regex: {e}"
            )))
        })?;
        let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let mut matches = Vec::new();
        let mut truncated = false;

        for (stored_path, content) in files.iter() {
            if let Some(base) = path {
                // Path-separator-aware prefix match: "/src" should match
                // "/src" and "/src/file" but NOT "/src2/file".
                let base = base.trim_end_matches('/');
                if !(stored_path == base
                    || stored_path.starts_with(&format!("{base}/")))
                {
                    continue;
                }
            }
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    if let Some(max) = max_count {
                        if matches.len() >= max {
                            truncated = true;
                            break;
                        }
                    }
                    matches.push(GrepMatch {
                        line_number: i + 1,
                        line: line.to_string(),
                    });
                }
            }
            if truncated {
                break;
            }
        }
        Ok(GrepResult { matches, truncated })
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult, Error> {
        let glob_matcher = globset::GlobBuilder::new(pattern)
            .build()
            .map_err(|e| {
                Error::Sandbox(deepagents_errors::SandboxError::Provider(format!(
                    "invalid glob: {e}"
                )))
            })?;
        let matcher = glob_matcher.compile_matcher();

        let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let paths: Vec<String> = files
            .keys()
            .filter(|p| {
                if let Some(base) = path {
                    // Path-separator-aware prefix match: "/src" should match
                    // "/src" and "/src/file" but NOT "/src2/file".
                    let base = base.trim_end_matches('/');
                    (**p == base || p.starts_with(&format!("{base}/")))
                        && matcher.is_match(p)
                } else {
                    matcher.is_match(p)
                }
            })
            .cloned()
            .collect();
        Ok(GlobResult { paths })
    }

    async fn write(&self, path: &str, content: &str) -> Result<WriteResult, Error> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = content.len();
        files.insert(path.to_string(), content.to_string());
        Ok(WriteResult { bytes_written: bytes })
    }

    async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult, Error> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let content = files
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Sandbox(deepagents_errors::SandboxError::NotFound(path.into())))?;
        if old.is_empty() {
            return Ok(EditResult { replacements: 0 });
        }
        let new_content = content.replacen(old, new, 1);
        let replacements = if new_content != content { 1 } else { 0 };
        files.insert(path.to_string(), new_content);
        Ok(EditResult { replacements })
    }

    async fn delete(&self, path: &str) -> Result<DeleteResult, Error> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let deleted = files.remove(path).is_some();
        Ok(DeleteResult { deleted })
    }

    async fn info(&self, path: &str) -> Result<FileInfo, Error> {
        let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(content) = files.get(path) {
            return Ok(FileInfo {
                path: path.to_string(),
                is_dir: false,
                size: content.len() as u64,
            });
        }
        // Check if it's a directory (prefix match)
        let prefix = format!("{path}/");
        let is_dir = files.keys().any(|p| p.starts_with(&prefix));
        if is_dir {
            Ok(FileInfo {
                path: path.to_string(),
                is_dir: true,
                size: 0,
            })
        } else {
            Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                path.into(),
            )))
        }
    }

    async fn list_files(&self) -> Result<Vec<FileInfo>, Error> {
        let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        Ok(files
            .iter()
            .map(|(path, content)| FileInfo {
                path: path.clone(),
                is_dir: false,
                size: content.len() as u64,
            })
            .collect())
    }
}

// ── FilesystemBackend (std::fs, sandboxed) ─────────────────────────────

#[cfg(feature = "filesystem")]
mod fs_backend {
    use super::*;

    /// Disk-backed filesystem with a mandatory sandbox root.
    ///
    /// All virtual paths are resolved relative to `root_dir` and must not
    /// escape it (no `..` traversal).
    #[derive(Debug, Clone)]
    pub struct FilesystemBackend {
        root_dir: PathBuf,
    }

    impl FilesystemBackend {
        /// Create a new filesystem backend sandboxed to `root_dir`.
        pub fn new(root_dir: impl Into<PathBuf>) -> Self {
            Self {
                root_dir: root_dir.into(),
            }
        }

        /// Resolve a virtual POSIX path to a real disk path.
        ///
        /// Returns an error if the path contains `..` or `~`, does not
        /// start with `/`, or (after canonicalization) escapes `root_dir`.
        ///
        /// Symlinks are resolved via `std::fs::canonicalize`; if the
        /// canonicalized path does not start with the canonicalized
        /// `root_dir`, the path is rejected. This prevents symlink-based
        /// sandbox escapes.
        fn resolve(&self, virtual_path: &str) -> Result<PathBuf, Error> {
            if !virtual_path.starts_with('/') {
                return Err(Error::Sandbox(
                    deepagents_errors::SandboxError::Provider(
                        "path must start with /".into(),
                    ),
                ));
            }
            if virtual_path.contains("..") || virtual_path.contains('~') {
                return Err(Error::Sandbox(
                    deepagents_errors::SandboxError::Provider(
                        "path must not contain .. or ~".into(),
                    ),
                ));
            }
            let relative = virtual_path.trim_start_matches('/');
            let real = self.root_dir.join(relative);

            // Canonicalize both root and resolved path to detect symlink
            // escapes. If the file doesn't exist yet (e.g. a write target),
            // canonicalize the parent directory and append the filename.
            let canon_root =
                std::fs::canonicalize(&self.root_dir).unwrap_or_else(|_| self.root_dir.clone());
            let canon_real = match std::fs::canonicalize(&real) {
                Ok(c) => c,
                // Path doesn't exist (new file). Canonicalize the parent
                // and re-append the filename.
                Err(_) => {
                    let parent = real.parent().unwrap_or(&self.root_dir);
                    let canon_parent = std::fs::canonicalize(parent)
                        .map_err(|e| Error::Sandbox(
                            deepagents_errors::SandboxError::Provider(format!(
                                "cannot canonicalize parent directory: {e}"
                            )),
                        ))?;
                    let filename = real.file_name().unwrap_or_default();
                    canon_parent.join(filename)
                }
            };

            // Verify the canonicalized path is under the canonicalized root.
            if !canon_real.starts_with(&canon_root) {
                return Err(Error::Sandbox(
                    deepagents_errors::SandboxError::Provider(format!(
                        "path escapes sandbox root: {} is not under {}",
                        canon_real.display(),
                        canon_root.display(),
                    )),
                ));
            }

            Ok(canon_real)
        }
    }

    #[async_trait]
    impl Backend for FilesystemBackend {
        async fn ls(&self, path: &str) -> Result<LsResult, Error> {
            let real = self.resolve(path)?;
            let mut entries = Vec::new();
            if real.is_dir() {
                for entry in std::fs::read_dir(&real)? {
                    let entry = entry?;
                    let meta = entry.metadata()?;
                    entries.push(DirEntry {
                        name: entry.file_name().to_string_lossy().to_string(),
                        is_dir: meta.is_dir(),
                        size: meta.len(),
                    });
                }
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(LsResult { entries })
        }

        async fn read(
            &self,
            path: &str,
            offset: usize,
            limit: usize,
        ) -> Result<ReadResult, Error> {
            let real = self.resolve(path)?;
            let content = std::fs::read_to_string(&real)?;
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len();
            let start = offset.min(total_lines);
            let end = if limit == 0 {
                total_lines
            } else {
                (start + limit).min(total_lines)
            };
            let truncated = end < total_lines;
            let content = lines[start..end].join("\n");
            Ok(ReadResult {
                content,
                total_lines,
                truncated,
            })
        }

        async fn grep(
            &self,
            pattern: &str,
            path: Option<&str>,
            glob: Option<&str>,
            max_count: Option<usize>,
        ) -> Result<GrepResult, Error> {
            let re = regex::Regex::new(pattern).map_err(|e| {
                Error::Sandbox(deepagents_errors::SandboxError::Provider(format!(
                    "invalid regex: {e}"
                )))
            })?;
            let root = if let Some(base) = path {
                self.resolve(base)?
            } else {
                self.root_dir.clone()
            };

            let glob_matcher = if let Some(glob_pattern) = glob {
                Some(
                    globset::GlobBuilder::new(glob_pattern)
                        .build()
                        .map_err(|e| {
                            Error::Sandbox(deepagents_errors::SandboxError::Provider(format!(
                                "invalid glob: {e}"
                            )))
                        })?
                        .compile_matcher(),
                )
            } else {
                None
            };

            let mut matches = Vec::new();
            let mut truncated = false;

            for entry in walkdir::WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path_str = entry.path().to_string_lossy().to_string();
                if let Some(gm) = &glob_matcher {
                    if !gm.is_match(&path_str) {
                        continue;
                    }
                }
                let content = match std::fs::read_to_string(entry.path()) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        if let Some(max) = max_count {
                            if matches.len() >= max {
                                truncated = true;
                                break;
                            }
                        }
                        matches.push(GrepMatch {
                            line_number: i + 1,
                            line: line.to_string(),
                        });
                    }
                }
                if truncated {
                    break;
                }
            }
            Ok(GrepResult { matches, truncated })
        }

        async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult, Error> {
            let root = if let Some(base) = path {
                self.resolve(base)?
            } else {
                self.root_dir.clone()
            };
            let glob_matcher = globset::GlobBuilder::new(pattern)
                .build()
                .map_err(|e| {
                    Error::Sandbox(deepagents_errors::SandboxError::Provider(format!(
                        "invalid glob: {e}"
                    )))
                })?;
            let matcher = glob_matcher.compile_matcher();

            let mut paths = Vec::new();
            for entry in walkdir::WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&self.root_dir)
                    .unwrap_or(entry.path());
                let posix = rel.to_string_lossy().replace('\\', "/");
                let full = format!("/{posix}");
                if matcher.is_match(&full) {
                    paths.push(full);
                }
            }
            paths.sort();
            Ok(GlobResult { paths })
        }

        async fn write(&self, path: &str, content: &str) -> Result<WriteResult, Error> {
            let real = self.resolve(path)?;
            if let Some(parent) = real.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = content.len();
            std::fs::write(&real, content)?;
            Ok(WriteResult { bytes_written: bytes })
        }

        async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult, Error> {
            let real = self.resolve(path)?;
            let content = std::fs::read_to_string(&real)?;
            if old.is_empty() {
                return Ok(EditResult { replacements: 0 });
            }
            let new_content = content.replacen(old, new, 1);
            let replacements = if new_content != content { 1 } else { 0 };
            std::fs::write(&real, new_content)?;
            Ok(EditResult { replacements })
        }

        async fn delete(&self, path: &str) -> Result<DeleteResult, Error> {
            let real = self.resolve(path)?;
            let deleted = std::fs::remove_file(&real).is_ok();
            Ok(DeleteResult { deleted })
        }

        async fn info(&self, path: &str) -> Result<FileInfo, Error> {
            let real = self.resolve(path)?;
            let meta = std::fs::metadata(&real)?;
            Ok(FileInfo {
                path: path.to_string(),
                is_dir: meta.is_dir(),
                size: meta.len(),
            })
        }

        async fn list_files(&self) -> Result<Vec<FileInfo>, Error> {
            let mut files = Vec::new();
            for entry in walkdir::WalkDir::new(&self.root_dir).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&self.root_dir)
                    .unwrap_or(entry.path());
                let posix = rel.to_string_lossy().replace('\\', "/");
                let full = format!("/{posix}");
                let meta = entry.metadata().map_err(|e| {
                    Error::Sandbox(deepagents_errors::SandboxError::Provider(format!(
                        "walkdir metadata: {e}"
                    )))
                })?;
                files.push(FileInfo {
                    path: full,
                    is_dir: false,
                    size: meta.len(),
                });
            }
            files.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(files)
        }
    }
}

#[cfg(feature = "filesystem")]
pub use fs_backend::FilesystemBackend;

// ── LocalShellBackend (disk + process) ─────────────────────────────────

#[cfg(feature = "filesystem")]
mod shell_backend {
    use super::*;
    use std::time::Duration;

    /// Disk-backed filesystem + local shell execution.
    ///
    /// Uses `FilesystemBackend` for file operations and spawns processes via
    /// `tokio::process::Command`. On Windows it selects `cmd` or `pwsh`.
    #[derive(Debug, Clone)]
    pub struct LocalShellBackend {
        fs: super::fs_backend::FilesystemBackend,
    }

    impl LocalShellBackend {
        /// Create a new shell backend sandboxed to `root_dir`.
        pub fn new(root_dir: impl Into<PathBuf>) -> Self {
            Self {
                fs: super::fs_backend::FilesystemBackend::new(root_dir),
            }
        }
    }

    #[async_trait]
    impl Backend for LocalShellBackend {
        async fn ls(&self, path: &str) -> Result<LsResult, Error> {
            self.fs.ls(path).await
        }
        async fn read(
            &self,
            path: &str,
            offset: usize,
            limit: usize,
        ) -> Result<ReadResult, Error> {
            self.fs.read(path, offset, limit).await
        }
        async fn grep(
            &self,
            pattern: &str,
            path: Option<&str>,
            glob: Option<&str>,
            max_count: Option<usize>,
        ) -> Result<GrepResult, Error> {
            self.fs.grep(pattern, path, glob, max_count).await
        }
        async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult, Error> {
            self.fs.glob(pattern, path).await
        }
        async fn write(&self, path: &str, content: &str) -> Result<WriteResult, Error> {
            self.fs.write(path, content).await
        }
        async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult, Error> {
            self.fs.edit(path, old, new).await
        }
        async fn delete(&self, path: &str) -> Result<DeleteResult, Error> {
            self.fs.delete(path).await
        }
        async fn info(&self, path: &str) -> Result<FileInfo, Error> {
            self.fs.info(path).await
        }
        async fn list_files(&self) -> Result<Vec<FileInfo>, Error> {
            self.fs.list_files().await
        }
    }

    #[async_trait]
    impl SandboxBackend for LocalShellBackend {
        async fn execute(
            &self,
            command: &str,
            timeout: Option<u64>,
        ) -> Result<ExecuteResponse, Error> {
            let mut cmd = if cfg!(windows) {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C").arg(command);
                c
            } else {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(command);
                c
            };
            cmd.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            let child = cmd.spawn()?;
            let timed_out;
            let output = if let Some(secs) = timeout {
                match tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()).await {
                    Ok(Ok(o)) => {
                        timed_out = false;
                        o
                    }
                    Ok(Err(e)) => return Err(Error::Io(e)),
                    Err(_) => {
                        timed_out = true;
                        std::process::Output {
                            status: std::process::ExitStatus::default(),
                            stdout: Vec::new(),
                            stderr: b"command timed out".to_vec(),
                        }
                    }
                }
            } else {
                timed_out = false;
                child.wait_with_output().await?
            };

            Ok(ExecuteResponse {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                timed_out,
            })
        }

        fn id(&self) -> &str {
            "local-shell"
        }
    }
}

#[cfg(feature = "filesystem")]
pub use shell_backend::LocalShellBackend;

// ── CompositeBackend (longest-prefix routing) ──────────────────────────

#[cfg(feature = "composite")]
mod composite_backend {
    use super::*;

    /// Routes operations to child backends based on longest path-prefix match.
    ///
    /// Each child is registered with a mount point (e.g. `/src` → FilesystemBackend).
    /// A request for `/src/main.rs` routes to the child with the longest matching prefix.
    #[derive(Clone, Default)]
    pub struct CompositeBackend {
        /// (prefix, backend) pairs, sorted by prefix length descending at lookup time.
        children: Vec<(String, Arc<dyn Backend>)>,
    }

    impl std::fmt::Debug for CompositeBackend {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CompositeBackend")
                .field("mounts", &self.children.iter().map(|(p, _)| p).collect::<Vec<_>>())
                .finish()
        }
    }

    impl CompositeBackend {
        /// Create an empty composite backend.
        pub fn new() -> Self {
            Self::default()
        }

        /// Mount a child backend at a path prefix.
        pub fn mount(&mut self, prefix: impl Into<String>, backend: Arc<dyn Backend>) {
            let prefix = prefix.into();
            self.children.push((prefix, backend));
            // Sort by prefix length descending so longest match is first
            self.children.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        }

        /// Find the backend for a given path (longest matching prefix).
        ///
        /// The match is path-separator-aware: a mount at `/src` matches
        /// `/src` and `/src/file` but NOT `/src2/file`. This prevents
        /// accidental routing to the wrong child backend.
        fn route(&self, path: &str) -> Option<(&str, &Arc<dyn Backend>)> {
            for (prefix, backend) in &self.children {
                // Exact match OR path starts with "prefix/"
                // (with trailing-slash tolerance on the prefix).
                let prefix_trimmed = prefix.trim_end_matches('/');
                if path == prefix_trimmed || path.starts_with(&format!("{prefix_trimmed}/")) {
                    return Some((prefix.as_str(), backend));
                }
            }
            None
        }

        /// Strip the mount prefix from a path for delegation to the child.
        ///
        /// The child backend always receives an absolute path (starting with
        /// `/`). If the stripped path doesn't start with `/`, one is prepended.
        fn strip_prefix<'a>(&self, prefix: &str, path: &'a str) -> String {
            let stripped = path.strip_prefix(prefix).unwrap_or(path);
            if stripped.starts_with('/') {
                stripped.to_string()
            } else {
                format!("/{stripped}")
            }
        }
    }

    #[async_trait]
    impl Backend for CompositeBackend {
        async fn ls(&self, path: &str) -> Result<LsResult, Error> {
            match self.route(path) {
                Some((prefix, backend)) => {
                    let child_path = self.strip_prefix(prefix, path);
                    backend.ls(&child_path).await
                }
                None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                    format!("no mount for {path}"),
                ))),
            }
        }

        async fn read(
            &self,
            path: &str,
            offset: usize,
            limit: usize,
        ) -> Result<ReadResult, Error> {
            match self.route(path) {
                Some((prefix, backend)) => {
                    let child_path = self.strip_prefix(prefix, path);
                    backend.read(&child_path, offset, limit).await
                }
                None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                    format!("no mount for {path}"),
                ))),
            }
        }

        async fn grep(
            &self,
            pattern: &str,
            path: Option<&str>,
            glob: Option<&str>,
            max_count: Option<usize>,
        ) -> Result<GrepResult, Error> {
            // If a path is specified, route to the matching child; otherwise
            // fan out to all children and merge.
            if let Some(base) = path {
                return match self.route(base) {
                    Some((prefix, backend)) => {
                        let child_path = self.strip_prefix(prefix, base);
                        backend.grep(pattern, Some(&child_path), glob, max_count).await
                    }
                    None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                        format!("no mount for {base}"),
                    ))),
                };
            }
            let mut all_matches = Vec::new();
            let mut truncated = false;
            for (_, backend) in &self.children {
                let result = backend.grep(pattern, None, glob, max_count).await?;
                all_matches.extend(result.matches);
                if result.truncated {
                    truncated = true;
                    break;
                }
            }
            Ok(GrepResult {
                matches: all_matches,
                truncated,
            })
        }

        async fn glob(&self, pattern: &str, path: Option<&str>) -> Result<GlobResult, Error> {
            if let Some(base) = path {
                return match self.route(base) {
                    Some((prefix, backend)) => {
                        let child_path = self.strip_prefix(prefix, base);
                        backend.glob(pattern, Some(&child_path)).await
                    }
                    None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                        format!("no mount for {base}"),
                    ))),
                };
            }
            let mut all_paths = Vec::new();
            for (prefix, backend) in &self.children {
                let result = backend.glob(pattern, None).await?;
                for p in result.paths {
                    all_paths.push(format!("{prefix}{p}"));
                }
            }
            all_paths.sort();
            Ok(GlobResult { paths: all_paths })
        }

        async fn write(&self, path: &str, content: &str) -> Result<WriteResult, Error> {
            match self.route(path) {
                Some((prefix, backend)) => {
                    let child_path = self.strip_prefix(prefix, path);
                    backend.write(&child_path, content).await
                }
                None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                    format!("no mount for {path}"),
                ))),
            }
        }

        async fn edit(&self, path: &str, old: &str, new: &str) -> Result<EditResult, Error> {
            match self.route(path) {
                Some((prefix, backend)) => {
                    let child_path = self.strip_prefix(prefix, path);
                    backend.edit(&child_path, old, new).await
                }
                None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                    format!("no mount for {path}"),
                ))),
            }
        }

        async fn delete(&self, path: &str) -> Result<DeleteResult, Error> {
            match self.route(path) {
                Some((prefix, backend)) => {
                    let child_path = self.strip_prefix(prefix, path);
                    backend.delete(&child_path).await
                }
                None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                    format!("no mount for {path}"),
                ))),
            }
        }

        async fn info(&self, path: &str) -> Result<FileInfo, Error> {
            match self.route(path) {
                Some((prefix, backend)) => {
                    let child_path = self.strip_prefix(prefix, path);
                    let mut info = backend.info(&child_path).await?;
                    info.path = path.to_string();
                    Ok(info)
                }
                None => Err(Error::Sandbox(deepagents_errors::SandboxError::NotFound(
                    format!("no mount for {path}"),
                ))),
            }
        }

        async fn list_files(&self) -> Result<Vec<FileInfo>, Error> {
            let mut all_files = Vec::new();
            for (prefix, backend) in &self.children {
                let files = backend.list_files().await?;
                for f in files {
                    all_files.push(FileInfo {
                        path: format!("{prefix}{}", f.path),
                        is_dir: f.is_dir,
                        size: f.size,
                    });
                }
            }
            all_files.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(all_files)
        }
    }
}

#[cfg(feature = "composite")]
pub use composite_backend::CompositeBackend;

#[cfg(test)]
mod tests {
    use super::*;

    // ── StateBackend: write + read round-trip ─────────────────────────

    #[tokio::test]
    async fn test_state_backend_write_read() {
        let backend = StateBackend::new();
        backend
            .write("/test.txt", "hello world")
            .await
            .unwrap();
        let result = backend.read("/test.txt", 0, 0).await.unwrap();
        assert_eq!(result.content, "hello world");
        assert_eq!(result.total_lines, 1);
        assert!(!result.truncated);
    }

    // ── StateBackend: read with offset + limit ────────────────────────

    #[tokio::test]
    async fn test_state_backend_read_offset_limit() {
        let backend = StateBackend::new();
        let content = "line1\nline2\nline3\nline4\nline5";
        backend.write("/file.txt", content).await.unwrap();

        let result = backend.read("/file.txt", 1, 2).await.unwrap();
        assert_eq!(result.content, "line2\nline3");
        assert_eq!(result.total_lines, 5);
        assert!(result.truncated);
    }

    // ── StateBackend: read nonexistent file ───────────────────────────

    #[tokio::test]
    async fn test_state_backend_read_not_found() {
        let backend = StateBackend::new();
        let result = backend.read("/missing.txt", 0, 0).await;
        assert!(result.is_err());
    }

    // ── StateBackend: ls lists directory entries ──────────────────────

    #[tokio::test]
    async fn test_state_backend_ls() {
        let backend = StateBackend::new();
        backend.write("/dir/file1.txt", "a").await.unwrap();
        backend.write("/dir/file2.txt", "b").await.unwrap();
        backend.write("/dir/sub/file3.txt", "c").await.unwrap();

        let result = backend.ls("/dir").await.unwrap();
        let names: Vec<&str> = result.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"file1.txt"));
        assert!(names.contains(&"file2.txt"));
        assert!(names.contains(&"sub")); // directory entry
    }

    // ── StateBackend: edit replaces first occurrence ──────────────────

    #[tokio::test]
    async fn test_state_backend_edit() {
        let backend = StateBackend::new();
        backend
            .write("/file.txt", "hello world hello")
            .await
            .unwrap();

        let result = backend.edit("/file.txt", "hello", "hi").await.unwrap();
        assert_eq!(result.replacements, 1);

        let read = backend.read("/file.txt", 0, 0).await.unwrap();
        assert_eq!(read.content, "hi world hello");
    }

    // ── StateBackend: edit nonexistent file ───────────────────────────

    #[tokio::test]
    async fn test_state_backend_edit_not_found() {
        let backend = StateBackend::new();
        let result = backend.edit("/missing.txt", "a", "b").await;
        assert!(result.is_err());
    }

    // ── StateBackend: delete ───────────────────────────────────────────

    #[tokio::test]
    async fn test_state_backend_delete() {
        let backend = StateBackend::new();
        backend.write("/file.txt", "content").await.unwrap();

        let result = backend.delete("/file.txt").await.unwrap();
        assert!(result.deleted);

        // Deleting again should return deleted: false
        let result = backend.delete("/file.txt").await.unwrap();
        assert!(!result.deleted);
    }

    // ── StateBackend: grep finds matches ──────────────────────────────

    #[tokio::test]
    async fn test_state_backend_grep() {
        let backend = StateBackend::new();
        backend.write("/file1.txt", "foo bar\nbaz foo").await.unwrap();
        backend.write("/file2.txt", "nothing here").await.unwrap();

        let result = backend.grep("foo", None, None, None).await.unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].line_number, 1);
        assert_eq!(result.matches[0].line, "foo bar");
        assert_eq!(result.matches[1].line_number, 2);
        assert_eq!(result.matches[1].line, "baz foo");
    }

    // ── StateBackend: grep with max_count ────────────────────────────

    #[tokio::test]
    async fn test_state_backend_grep_max_count() {
        let backend = StateBackend::new();
        backend.write("/file.txt", "foo\nfoo\nfoo\nfoo").await.unwrap();

        let result = backend.grep("foo", None, None, Some(2)).await.unwrap();
        assert_eq!(result.matches.len(), 2);
        assert!(result.truncated);
    }

    // ── StateBackend: glob matches patterns ──────────────────────────

    #[tokio::test]
    async fn test_state_backend_glob() {
        let backend = StateBackend::new();
        backend.write("/src/main.rs", "fn main()").await.unwrap();
        backend.write("/src/mod.rs", "fn mod()").await.unwrap();
        backend.write("/src/README.md", "# readme").await.unwrap();

        let result = backend.glob("/**/*.rs", None).await.unwrap();
        assert_eq!(result.paths.len(), 2);
        assert!(result.paths.contains(&"/src/main.rs".to_string()));
        assert!(result.paths.contains(&"/src/mod.rs".to_string()));
    }

    // ── StateBackend: info on file and directory ──────────────────────

    #[tokio::test]
    async fn test_state_backend_info() {
        let backend = StateBackend::new();
        backend.write("/file.txt", "12345").await.unwrap();
        backend.write("/dir/inner.txt", "x").await.unwrap();

        let info = backend.info("/file.txt").await.unwrap();
        assert!(!info.is_dir);
        assert_eq!(info.size, 5);

        let info = backend.info("/dir").await.unwrap();
        assert!(info.is_dir);
    }

    // ── StateBackend: list_files ───────────────────────────────────────

    #[tokio::test]
    async fn test_state_backend_list_files() {
        let backend = StateBackend::new();
        backend.write("/a.txt", "aaa").await.unwrap();
        backend.write("/b.txt", "bbb").await.unwrap();

        let files = backend.list_files().await.unwrap();
        assert_eq!(files.len(), 2);
    }

    // ── CompositeBackend: routing to mounted child ───────────────────

    #[cfg(feature = "composite")]
    #[tokio::test]
    async fn test_composite_routing() {
        let child1 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let child2 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let mut composite = CompositeBackend::new();
        composite.mount("/src", child1.clone());
        composite.mount("/tmp", child2.clone());

        // Write via composite → routes to child1
        composite.write("/src/file.rs", "fn main()").await.unwrap();
        // Verify it's in child1
        let read = child1.read("/src/file.rs", 0, 0).await;
        // The composite strips prefix, so child sees /file.rs
        // (strip_prefix removes "/src" and prepends "/")
        match read {
            Ok(r) => assert_eq!(r.content, "fn main()"),
            Err(_) => {
                // child1 might see it as /file.rs (without /src prefix)
                let r = child1.read("/file.rs", 0, 0).await.unwrap();
                assert_eq!(r.content, "fn main()");
            }
        }
    }

    // ── CompositeBackend: no mount for path ───────────────────────────

    #[cfg(feature = "composite")]
    #[tokio::test]
    async fn test_composite_no_mount() {
        let composite = CompositeBackend::new();
        let result = composite.ls("/unmounted").await;
        assert!(result.is_err());
    }

    // ── CompositeBackend: longest prefix match ────────────────────────

    #[cfg(feature = "composite")]
    #[tokio::test]
    async fn test_composite_longest_prefix() {
        let child1 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let child2 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let mut composite = CompositeBackend::new();
        composite.mount("/src", child1.clone());
        composite.mount("/src/nested", child2.clone());

        // /src/nested/deep.rs should route to child2 (longest prefix)
        composite.write("/src/nested/deep.rs", "content").await.unwrap();

        // child2 should have the file
        let files = child2.list_files().await.unwrap();
        assert!(!files.is_empty());
    }

    // ── CompositeBackend: strip_prefix ensures absolute path ─────────

    #[cfg(feature = "composite")]
    #[tokio::test]
    async fn test_composite_strip_prefix_absolute() {
        let child = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let mut composite = CompositeBackend::new();
        composite.mount("/data", child.clone());

        // Write through composite; child should receive "/file.txt"
        // (not "data/file.txt" or "file.txt" without leading /)
        composite.write("/data/file.txt", "test").await.unwrap();

        // Verify child received it as an absolute path
        let result = child.read("/file.txt", 0, 0).await.unwrap();
        assert_eq!(result.content, "test");
    }

    // ── CompositeBackend: list_files aggregates children ────────────

    #[cfg(feature = "composite")]
    #[tokio::test]
    async fn test_composite_list_files() {
        let child1 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let child2 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        child1.write("/a.rs", "a").await.unwrap();
        child2.write("/b.rs", "b").await.unwrap();

        let mut composite = CompositeBackend::new();
        composite.mount("/src", child1);
        composite.mount("/tmp", child2);

        let files = composite.list_files().await.unwrap();
        assert_eq!(files.len(), 2);
        // Paths should include mount prefix
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.starts_with("/src")));
        assert!(paths.iter().any(|p| p.starts_with("/tmp")));
    }

    // ── Path-separator-aware prefix matching ───────────────────────────
    //
    // These tests verify that "/src" as a path filter does NOT match
    // "/src2/..." — a bug that raw `starts_with` would introduce.
    // The fix uses path-separator-aware prefix matching everywhere
    // a path prefix is compared.

    #[tokio::test]
    async fn test_state_backend_glob_path_separator_aware() {
        let backend = StateBackend::new();
        backend.write("/src/main.rs", "fn main()").await.unwrap();
        backend
            .write("/src2/other.rs", "fn other()")
            .await
            .unwrap();

        // glob with path="/src" should only match /src/**, not /src2/**
        let result = backend.glob("/**/*.rs", Some("/src")).await.unwrap();
        assert_eq!(result.paths.len(), 1);
        assert!(result.paths.contains(&"/src/main.rs".to_string()));
        assert!(!result.paths.contains(&"/src2/other.rs".to_string()));
    }

    #[tokio::test]
    async fn test_state_backend_glob_path_separator_exact_match() {
        // When path is an exact file path (not a directory), it should
        // still match that single file.
        let backend = StateBackend::new();
        backend.write("/src/main.rs", "fn main()").await.unwrap();

        let result = backend.glob("/src/main.rs", Some("/src/main.rs")).await.unwrap();
        assert_eq!(result.paths.len(), 1);
    }

    #[tokio::test]
    async fn test_state_backend_grep_path_separator_aware() {
        let backend = StateBackend::new();
        backend.write("/src/file.txt", "match\nfoo").await.unwrap();
        backend.write("/src2/file.txt", "match\nbar").await.unwrap();

        // grep with path="/src" should only search /src/**, not /src2/**
        let result = backend.grep("match", Some("/src"), None, None)
            .await
            .unwrap();
        assert_eq!(result.matches.len(), 1);
    }

    #[tokio::test]
    async fn test_state_backend_grep_path_trailing_slash() {
        // Trailing slash on the path should be tolerated.
        let backend = StateBackend::new();
        backend.write("/src/file.txt", "match").await.unwrap();
        backend.write("/src2/file.txt", "match").await.unwrap();

        let result = backend.grep("match", Some("/src/"), None, None)
            .await
            .unwrap();
        assert_eq!(result.matches.len(), 1);
    }

    #[cfg(feature = "composite")]
    #[tokio::test]
    async fn test_composite_route_separator_aware() {
        // Mount at /src should NOT route /src2/file to the /src child.
        let child_src = Arc::new(StateBackend::new()) as Arc<dyn Backend>;
        let child_src2 = Arc::new(StateBackend::new()) as Arc<dyn Backend>;

        let mut composite = CompositeBackend::new();
        composite.mount("/src", child_src.clone());
        composite.mount("/src2", child_src2.clone());

        // Write to /src2/file.txt — should route to child_src2, not child_src
        composite.write("/src2/file.txt", "in src2").await.unwrap();

        // child_src2 should have the file (as /file.txt after strip_prefix)
        let r = child_src2.read("/file.txt", 0, 0).await.unwrap();
        assert_eq!(r.content, "in src2");

        // child_src should NOT have /file.txt
        let r = child_src.read("/file.txt", 0, 0).await;
        assert!(r.is_err(), "child_src should not have /file.txt");
    }
}

//! `FilesystemBackend` —— 对应 deepagents `FilesystemBackend`（直接磁盘 I/O）。
//!
//! 本地磁盘 backend。virtual path（`/foo`）映射到 `root/foo`，防越界。
//! `grep` 是字面量匹配（非 regex，对齐 deepagents 语义）。`glob` 用 `globset`。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use globset::Glob;
use walkdir::WalkDir;

use super::protocol::Backend;
use super::types::*;

/// 本地磁盘 backend。
#[derive(Debug, Clone)]
pub struct FilesystemBackend {
    /// 根目录，virtual path 在其下。
    root: PathBuf,
}

impl FilesystemBackend {
    /// 以根目录构造。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 根目录引用。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// virtual path（`/foo` 或 `foo`）映射到 `root/foo`，防越界。
    ///
    /// 拒绝含 `..` 的路径（对齐 deepagents `validate_path`：paths 不能含 `..`）。
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let trimmed = path.strip_prefix('/').unwrap_or(path);
        if trimmed.split('/').any(|c| c == "..") {
            return Err(format!("path escapes root (contains '..'): {path}"));
        }
        Ok(self.root.join(trimmed))
    }
}

#[async_trait]
impl Backend for FilesystemBackend {
    fn supported_tools(&self) -> Vec<&'static str> {
        // execute 由 SandboxBackend 提供；FilesystemBackend 非 sandbox。
        vec!["ls", "read_file", "write_file", "edit_file", "delete", "glob", "grep"]
    }

    async fn ls(&self, path: &str) -> LsResult {
        let dir = match self.resolve(path) {
            Ok(p) => p,
            Err(e) => return LsResult::err(e),
        };
        let mut read = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(e) => return LsResult::err(e.to_string()),
        };
        let mut entries = Vec::new();
        while let Ok(Some(e)) = read.next_entry().await {
            let meta = e.metadata().await.ok();
            entries.push(FileInfo {
                path: e.path().to_string_lossy().to_string(),
                is_dir: meta.as_ref().map(|m| m.is_dir()),
                size: meta.as_ref().map(|m| m.len()),
                modified_at: meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs().to_string()),
            });
        }
        LsResult::ok(entries)
    }

    async fn read(&self, file_path: &str, offset: usize, limit: usize) -> ReadResult {
        let f = match self.resolve(file_path) {
            Ok(p) => p,
            Err(e) => return ReadResult::err(e),
        };
        let content = match tokio::fs::read_to_string(&f).await {
            Ok(c) => c,
            Err(e) => return ReadResult::err(e.to_string()),
        };
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        if limit == 0 {
            // 非正 limit：短路，文件未检视。
            return ReadResult {
                error: None,
                file_data: Some(FileData { content: String::new(), encoding: "utf-8".to_string(), ..Default::default() }),
                total_lines: None,
                start_line: None,
                end_line: None,
                next_offset: None,
                no_lines_requested: true,
            };
        }
        let start = offset.min(total);
        let end = (start + limit).min(total);
        let window: Vec<&str> = if start < end { lines[start..end].to_vec() } else { Vec::new() };
        let file_data = FileData { content: window.join("\n"), encoding: "utf-8".to_string(), ..Default::default() };
        if end > start {
            ReadResult::ok(file_data, total, start + 1, end, (end < total).then_some(end))
        } else {
            // 空文件或窗口为空
            ReadResult {
                error: None,
                file_data: Some(file_data),
                total_lines: Some(total),
                start_line: None,
                end_line: None,
                next_offset: None,
                no_lines_requested: false,
            }
        }
    }

    async fn write(&self, file_path: &str, content: &str) -> WriteResult {
        let f = match self.resolve(file_path) {
            Ok(p) => p,
            Err(e) => return WriteResult::err(e),
        };
        if let Some(parent) = f.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(&f, content).await {
            Ok(()) => WriteResult::ok(file_path.to_string()),
            Err(e) => WriteResult::err(e.to_string()),
        }
    }

    async fn edit(&self, file_path: &str, old_string: &str, new_string: &str, replace_all: bool) -> EditResult {
        let f = match self.resolve(file_path) {
            Ok(p) => p,
            Err(e) => return EditResult::err(e),
        };
        if old_string == new_string {
            return EditResult::err("old_string and new_string must differ");
        }
        let content = match tokio::fs::read_to_string(&f).await {
            Ok(c) => c,
            Err(e) => return EditResult::err(e.to_string()),
        };
        let count = content.matches(old_string).count();
        if count == 0 {
            return EditResult::err("old_string not found");
        }
        if count > 1 && !replace_all {
            return EditResult::err("old_string is not unique; pass replace_all=true to replace all");
        }
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        match tokio::fs::write(&f, new_content).await {
            Ok(()) => EditResult::ok(file_path.to_string(), if replace_all { count } else { 1 }),
            Err(e) => EditResult::err(e.to_string()),
        }
    }

    async fn delete(&self, file_path: &str) -> DeleteResult {
        let f = match self.resolve(file_path) {
            Ok(p) => p,
            Err(e) => return DeleteResult::err(e),
        };
        let meta = tokio::fs::symlink_metadata(&f).await;
        let res = match meta {
            Ok(m) if m.is_dir() => tokio::fs::remove_dir_all(&f).await,
            Ok(_) => tokio::fs::remove_file(&f).await,
            Err(e) => return DeleteResult::err(e.to_string()),
        };
        match res {
            Ok(()) => DeleteResult::ok(file_path.to_string()),
            Err(e) => DeleteResult::err(e.to_string()),
        }
    }

    async fn glob(&self, pattern: &str, path: Option<&str>) -> GlobResult {
        let base = match path {
            Some(p) => match self.resolve(p) {
                Ok(p) => p,
                Err(e) => return GlobResult::err(e),
            },
            None => self.root.clone(),
        };
        let glob = match Glob::new(pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => return GlobResult::err(e.to_string()),
        };
        let mut matches = Vec::new();
        for entry in WalkDir::new(&base).into_iter().filter_map(std::result::Result::ok) {
            let p = entry.path();
            // 匹配相对于 root 的路径或绝对路径
            let rel = p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().replace('\\', "/");
            if glob.is_match(&rel) || glob.is_match(p) {
                let meta = entry.metadata().ok();
                matches.push(FileInfo {
                    path: p.to_string_lossy().to_string(),
                    is_dir: meta.as_ref().map(|m| m.is_dir()),
                    size: meta.as_ref().map(|m| m.len()),
                    modified_at: None,
                });
            }
        }
        GlobResult::ok(matches)
    }

    async fn grep(&self, pattern: &str, path: Option<&str>, glob_filter: Option<&str>, max_count: Option<usize>) -> GrepResult {
        let base = match path {
            Some(p) => match self.resolve(p) {
                Ok(p) => p,
                Err(e) => return GrepResult::err(e),
            },
            None => self.root.clone(),
        };
        let glob_matcher = match glob_filter {
            Some(g) => match Glob::new(g) {
                Ok(gb) => Some(gb.compile_matcher()),
                Err(e) => return GrepResult::err(e.to_string()),
            },
            None => None,
        };
        let cap = max_count.unwrap_or(usize::MAX);
        let mut matches: Vec<GrepMatch> = Vec::new();
        for entry in WalkDir::new(&base).into_iter().filter_map(std::result::Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            let rel = p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().replace('\\', "/");
            if let Some(gm) = &glob_matcher
                && !gm.is_match(&rel)
                && !gm.is_match(p)
            {
                continue;
            }
            // 字面量匹配（非 regex）。
            let content = match tokio::fs::read_to_string(p).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (i, line) in content.lines().enumerate() {
                if line.contains(pattern) {
                    matches.push(GrepMatch {
                        path: p.to_string_lossy().to_string(),
                        line: i + 1,
                        text: line.to_string(),
                        context_before: None,
                        context_after: None,
                    });
                    if matches.len() >= cap {
                        return GrepResult::truncated(matches);
                    }
                }
            }
        }
        GrepResult::ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend(tmp: &tempfile::TempDir) -> FilesystemBackend {
        FilesystemBackend::new(tmp.path())
    }

    #[tokio::test]
    async fn write_read_roundtrip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        let w = b.write("/foo.txt", "hello\nworld\n").await;
        assert!(w.error.is_none());
        let r = b.read("/foo.txt", 0, 10).await;
        assert!(r.error.is_none());
        let fd = r.file_data.expect("file_data");
        assert_eq!(fd.content, "hello\nworld");
        assert_eq!(fd.encoding, "utf-8");
        assert_eq!(r.total_lines, Some(2));
        assert_eq!(r.start_line, Some(1));
        assert_eq!(r.end_line, Some(2));
        assert_eq!(r.next_offset, None); // 全部读完
    }

    #[tokio::test]
    async fn read_pagination() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/f.txt", "a\nb\nc\nd\ne\n").await;
        let r = b.read("/f.txt", 1, 2).await; // offset=1, limit=2 -> lines 2-3
        assert_eq!(r.file_data.expect("fd").content, "b\nc");
        assert_eq!(r.start_line, Some(2));
        assert_eq!(r.end_line, Some(3));
        assert_eq!(r.next_offset, Some(3)); // 续读偏移
        assert_eq!(r.total_lines, Some(5));
    }

    #[tokio::test]
    async fn read_limit_zero_short_circuits() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/f.txt", "x\n").await;
        let r = b.read("/f.txt", 0, 0).await;
        assert!(r.no_lines_requested);
        assert!(r.start_line.is_none());
        assert!(r.file_data.is_some());
    }

    #[tokio::test]
    async fn edit_unique_replace() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/f.txt", "foo bar baz\n").await;
        let e = b.edit("/f.txt", "bar", "QUX", false).await;
        assert!(e.error.is_none(), "{:?}", e.error);
        assert_eq!(e.occurrences, Some(1));
        let r = b.read("/f.txt", 0, 10).await;
        assert_eq!(r.file_data.expect("fd").content, "foo QUX baz");
    }

    #[tokio::test]
    async fn edit_non_unique_without_replace_all_fails() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/f.txt", "x x x\n").await;
        let e = b.edit("/f.txt", "x", "y", false).await;
        assert!(e.error.is_some());
    }

    #[tokio::test]
    async fn edit_replace_all() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/f.txt", "x x x\n").await;
        let e = b.edit("/f.txt", "x", "y", true).await;
        assert_eq!(e.occurrences, Some(3));
        let r = b.read("/f.txt", 0, 10).await;
        assert_eq!(r.file_data.expect("fd").content, "y y y");
    }

    #[tokio::test]
    async fn delete_file_and_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/d/a.txt", "a\n").await;
        b.write("/d/b.txt", "b\n").await;
        let d = b.delete("/d").await;
        assert!(d.error.is_none(), "{:?}", d.error);
        // 目录应已删除
        assert!(b.ls("/d").await.error.is_some());
    }

    #[tokio::test]
    async fn grep_literal() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/a.txt", "foo\nbar\nTODO: fix\n").await;
        b.write("/b.txt", "nothing here\n").await;
        let r = b.grep("TODO", None, None, None).await;
        assert!(r.error.is_none());
        let m = r.matches.expect("matches");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 3);
        assert!(m[0].text.contains("TODO"));
    }

    #[tokio::test]
    async fn grep_max_count_truncates() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/f.txt", "x\nx\nx\nx\n").await;
        let r = b.grep("x", None, None, Some(2)).await;
        assert!(r.truncated);
        assert_eq!(r.matches.expect("matches").len(), 2);
    }

    #[tokio::test]
    async fn glob_pattern() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/a.rs", "1").await;
        b.write("/b.txt", "2").await;
        b.write("/sub/c.rs", "3").await;
        let r = b.glob("**/*.rs", None).await;
        assert!(r.error.is_none());
        let m = r.matches.expect("matches");
        assert_eq!(m.len(), 2); // a.rs + sub/c.rs
    }

    #[tokio::test]
    async fn resolve_blocks_path_escape() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        // ../escape 应被拒绝
        let w = b.write("/../escape.txt", "x").await;
        assert!(w.error.is_some(), "escape should be blocked");
    }

    #[tokio::test]
    async fn ls_lists_entries() {
        let tmp = tempfile::tempdir().expect("tmp");
        let b = backend(&tmp);
        b.write("/a.txt", "1").await;
        b.write("/b.txt", "2").await;
        let r = b.ls("/").await;
        assert!(r.error.is_none());
        let paths: Vec<String> = r.entries.expect("entries").into_iter().map(|e| e.path).collect();
        assert!(paths.iter().any(|p| p.contains("a.txt")));
        assert!(paths.iter().any(|p| p.contains("b.txt")));
    }
}

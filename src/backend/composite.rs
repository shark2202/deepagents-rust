//! `CompositeBackend` —— 对应 deepagents `CompositeBackend`（按路径前缀路由到不同 backend）。
//!
//! 文件操作（ls/read/grep/glob/write/edit/delete）路由到最长前缀匹配的 route backend；
//! `execute` 走 `default` backend（通常 `LocalShellBackend`）。`supported_tools()` 返回
//! routes + default 的 union。`as_sandbox()` 返回 `default.as_sandbox()`。

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use super::protocol::{Backend, SandboxBackend};
use super::types::*;

/// 复合后端：按路径前缀路由。
#[derive(Clone)]
pub struct CompositeBackend {
    /// `(prefix, backend)` 路由表。前缀按 longest-match 优先。
    routes: Vec<(String, Arc<dyn Backend>)>,
    /// 默认后端（无路由匹配时使用；execute 走此）。
    default: Arc<dyn Backend>,
}

impl std::fmt::Debug for CompositeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeBackend")
            .field("routes", &self.routes.len())
            .finish_non_exhaustive()
    }
}

impl CompositeBackend {
    /// 构造：路由表 + 默认后端。
    #[must_use]
    pub fn new(routes: Vec<(String, Arc<dyn Backend>)>, default: Arc<dyn Backend>) -> Self {
        Self { routes, default }
    }

    /// 路由文件路径：longest 前缀匹配，无匹配 → default。
    fn route_for_path(&self, path: &str) -> &Arc<dyn Backend> {
        let mut best: Option<(&str, &Arc<dyn Backend>)> = None;
        for (prefix, b) in &self.routes {
            if path.starts_with(prefix.as_str())
                && best.as_ref().is_none_or(|(p, _)| prefix.len() > p.len())
            {
                best = Some((prefix, b));
            }
        }
        best.map(|(_, b)| b).unwrap_or(&self.default)
    }
}

#[async_trait]
impl Backend for CompositeBackend {
    fn supported_tools(&self) -> Vec<&'static str> {
        // union of routes + default
        let mut set: HashSet<&'static str> = HashSet::new();
        for (_, b) in &self.routes {
            for t in b.supported_tools() {
                set.insert(t);
            }
        }
        for t in self.default.supported_tools() {
            set.insert(t);
        }
        set.into_iter().collect()
    }

    async fn ls(&self, path: &str) -> LsResult {
        self.route_for_path(path).ls(path).await
    }
    async fn read(&self, file_path: &str, offset: usize, limit: usize) -> ReadResult {
        self.route_for_path(file_path)
            .read(file_path, offset, limit)
            .await
    }
    async fn write(&self, file_path: &str, content: &str) -> WriteResult {
        self.route_for_path(file_path)
            .write(file_path, content)
            .await
    }
    async fn edit(&self, file_path: &str, old: &str, new: &str, replace_all: bool) -> EditResult {
        self.route_for_path(file_path)
            .edit(file_path, old, new, replace_all)
            .await
    }
    async fn delete(&self, file_path: &str) -> DeleteResult {
        self.route_for_path(file_path).delete(file_path).await
    }
    async fn glob(&self, pattern: &str, path: Option<&str>) -> GlobResult {
        let base = path.unwrap_or("/");
        self.route_for_path(base).glob(pattern, path).await
    }
    async fn grep(
        &self,
        pattern: &str,
        path: Option<&str>,
        glob: Option<&str>,
        max_count: Option<usize>,
    ) -> GrepResult {
        let base = path.unwrap_or("/");
        self.route_for_path(base)
            .grep(pattern, path, glob, max_count)
            .await
    }

    fn as_sandbox(&self) -> Option<&dyn SandboxBackend> {
        // execute 走 default backend（通常 LocalShellBackend）。
        self.default.as_sandbox()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FilesystemBackend, LocalShellBackend};
    use super::*;

    #[tokio::test]
    async fn routes_file_ops_longest_prefix() {
        let tmp1 = tempfile::tempdir().expect("tmp1");
        let tmp2 = tempfile::tempdir().expect("tmp2");
        let fs_a = Arc::new(FilesystemBackend::new(tmp1.path())) as Arc<dyn Backend>;
        let fs_b = Arc::new(FilesystemBackend::new(tmp2.path())) as Arc<dyn Backend>;
        let local = Arc::new(LocalShellBackend::new()) as Arc<dyn Backend>;

        let cb = CompositeBackend::new(
            vec![
                ("/a".to_string(), fs_a),
                ("/a/sub".to_string(), fs_b), // longest prefix
            ],
            local,
        );

        // /a/x.txt 路由到 fs_a
        cb.write("/a/x.txt", "in a").await;
        assert!(tmp1.path().join("a/x.txt").exists());

        // /a/sub/y.txt 路由到 fs_b（longest prefix /a/sub）
        cb.write("/a/sub/y.txt", "in sub").await;
        assert!(tmp2.path().join("a/sub/y.txt").exists());
    }

    #[tokio::test]
    async fn execute_goes_to_default() {
        let tmp = tempfile::tempdir().expect("tmp");
        let fs = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;
        let local = Arc::new(LocalShellBackend::new()) as Arc<dyn Backend>;
        let cb = CompositeBackend::new(vec![("/".to_string(), fs)], local);

        // execute 走 default (LocalShellBackend)
        let sandbox = cb.as_sandbox().expect("default is sandbox");
        let r = sandbox.execute("echo composite", None).await;
        assert!(r.output.contains("composite"));
    }

    #[tokio::test]
    async fn supported_tools_is_union() {
        let tmp = tempfile::tempdir().expect("tmp");
        let fs = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;
        let local = Arc::new(LocalShellBackend::new()) as Arc<dyn Backend>;
        let cb = CompositeBackend::new(vec![("/".to_string(), fs)], local);

        let tools = cb.supported_tools();
        // union: 7 (fs) + 1 (execute) = 8
        assert!(tools.contains(&"read_file"));
        assert!(tools.contains(&"execute"));
        assert_eq!(tools.len(), 8);
    }
}

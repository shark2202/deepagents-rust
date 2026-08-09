//! `MemoryMiddleware` —— 把 AGENTS.md 记忆内容注入 system prompt。
//!
//! 对应 deepagents `MemoryMiddleware`：
//! - 持 `memory_sources: Vec<PathBuf>`（AGENTS.md 文件路径列表）
//! - `cached: Mutex<Option<String>>`（lazy 加载的合并内容，首次 `wrap_model_call` 时填充）
//! - `wrap_model_call`：首次调用 lazy 加载 → 逐文件读取 → strip HTML 注释 → trim → `\n\n` 合并
//!   → 注入 `req.system_message`：`<agent_memory>\n{content}\n</agent_memory>`
//! - `tools()` / `before_agent` / `after_model_call`：默认 no-op（记忆不提供工具、不碰 state）
//!
//! # 简化 / defer
//!
//! - 不做 vector store 语义检索（deepagents `Store` + 检索）—— defer
//! - 不做 namespace factory（按 namespace 组织记忆源）—— defer
//! - 仅注入静态 AGENTS.md 全文，每次调用都注入（缓存命中后零 IO）
//! - AGENTS.md 视为普通 markdown（agents.md spec），直接读全文；HTML 注释手动去除，不引入 regex 依赖

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::fs;

use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 记忆中间件：把 AGENTS.md 内容注入 system prompt（对应 deepagents `MemoryMiddleware`）。
///
/// 内容在首次 `wrap_model_call` 时 lazy 加载并缓存（`Mutex<Option<String>>`），
/// 之后每次调用直接从缓存注入（零 IO）。读取失败的单个文件被跳过（`tracing::warn`），
/// 不影响其余文件；若合并后为空，则不注入（避免空 `<agent_memory>` 块）。
pub struct MemoryMiddleware {
    /// AGENTS.md 文件路径列表（按配置顺序合并）。
    memory_sources: Vec<PathBuf>,
    /// lazy 加载的合并内容缓存；`None` = 尚未加载。
    cached: Mutex<Option<String>>,
}

impl std::fmt::Debug for MemoryMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cached_len = self
            .cached
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(String::len));
        f.debug_struct("MemoryMiddleware")
            .field("memory_sources", &self.memory_sources)
            .field("cached_len", &cached_len)
            .finish_non_exhaustive()
    }
}

impl MemoryMiddleware {
    /// 以给定 AGENTS.md 路径列表构造（不立刻读取，首次 `wrap_model_call` 时 lazy 加载）。
    #[must_use]
    pub fn new(memory_sources: Vec<PathBuf>) -> Self {
        Self {
            memory_sources,
            cached: Mutex::new(None),
        }
    }

    /// 已配置的记忆源路径。
    #[must_use]
    pub fn memory_sources(&self) -> &[PathBuf] {
        &self.memory_sources
    }

    /// 清空缓存，下次 `wrap_model_call` 重新读取（AGENTS.md 在运行期变更后调用）。
    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.cached.lock() {
            *guard = None;
        }
    }

    /// 逐文件读取 → strip HTML 注释 → trim → `\n\n` 合并。读取失败的文件被跳过（warn）。
    async fn load_and_merge(&self) -> Result<String, MiddlewareError> {
        let mut parts: Vec<String> = Vec::with_capacity(self.memory_sources.len());
        for path in &self.memory_sources {
            match fs::read_to_string(path).await {
                Ok(content) => {
                    let stripped = strip_html_comments(&content);
                    let trimmed = stripped.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_string());
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to read memory source {}; skipping: {e}", path.display());
                }
            }
        }
        Ok(parts.join("\n\n"))
    }
}

#[async_trait]
impl Middleware for MemoryMiddleware {
    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        // Fast path：缓存命中，直接注入。
        {
            let guard = self
                .cached
                .lock()
                .map_err(|e| MiddlewareError::Other(format!("memory cache lock poisoned: {e}")))?;
            if let Some(c) = guard.as_ref() {
                inject_memory(req, c);
                return Ok(());
            }
        }
        // Cache miss：加载 + 合并 + 注入 + 回填缓存。
        // 不在持锁期间 await，避免阻塞 executor（std Mutex 不跨 await）。
        let content = self.load_and_merge().await?;
        inject_memory(req, &content);
        if let Ok(mut guard) = self.cached.lock() {
            *guard = Some(content);
        }
        Ok(())
    }
}

/// 把合并内容作为 `<agent_memory>` 段追加到 system_message（与其它中间件段叠加）。
/// 内容为空时不注入（避免空块）。
fn inject_memory(req: &mut ModelRequest, content: &str) {
    if content.is_empty() {
        return;
    }
    req.system_message.push_str("\n\n<agent_memory>\n");
    req.system_message.push_str(content);
    req.system_message.push_str("\n</agent_memory>");
}

/// 手动去除 HTML 注释 `<!-- ... -->`（不引入 regex 依赖）。
///
/// 找到 `<!--` 后向后查 `-->`：命中则丢弃整段；未命中（未闭合）则丢弃到串尾
/// （与 HTML 解析器行为一致）。`<!--`/`-->` 为 ASCII，其位置必落在 UTF-8 字符边界，
/// 故切片安全。
fn strip_html_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 4..];
        match after_open.find("-->") {
            Some(end) => {
                rest = &after_open[end + 3..];
            }
            None => {
                // 未闭合注释：丢弃到串尾。
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use juncture::llm::CallOptions;

    #[test]
    fn strip_removes_closed_comment() {
        assert_eq!(strip_html_comments("before <!-- hidden --> after"), "before  after");
    }

    #[test]
    fn strip_removes_multiline_comment() {
        assert_eq!(strip_html_comments("a <!-- line1\nline2 --> b"), "a  b");
    }

    #[test]
    fn strip_drops_unclosed_comment_to_end() {
        assert_eq!(strip_html_comments("text <!-- never closed"), "text ");
    }

    #[test]
    fn strip_preserves_utf8_around_comments() {
        assert_eq!(strip_html_comments("记忆 <!-- 注释 --> 内容"), "记忆  内容");
    }

    #[test]
    fn strip_no_comments_unchanged() {
        let s = "plain markdown\n# heading";
        assert_eq!(strip_html_comments(s), s);
    }

    #[test]
    fn strip_multiple_comments() {
        assert_eq!(strip_html_comments("<!--a--> x <!--b--> y"), " x  y");
    }

    #[test]
    fn inject_appends_agent_memory_block() {
        let mut req = ModelRequest {
            tools: Vec::new(),
            system_message: "base".to_string(),
            options: CallOptions::default(),
        };
        inject_memory(&mut req, "记忆内容");
        assert_eq!(req.system_message, "base\n\n<agent_memory>\n记忆内容\n</agent_memory>");
    }

    #[test]
    fn inject_empty_is_noop() {
        let mut req = ModelRequest {
            tools: Vec::new(),
            system_message: "base".to_string(),
            options: CallOptions::default(),
        };
        inject_memory(&mut req, "");
        assert_eq!(req.system_message, "base");
    }

    // 注：完整 wrap_model_call lazy 加载 + 缓存路径的集成测试由主会话统一在
    // cargo test 编译验证时跑（需要 tempfile + tokio runtime，此处不重复）。
}

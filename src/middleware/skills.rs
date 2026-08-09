//! `SkillsMiddleware` —— 扫描 `skills_dir` 下的 `SKILL.md`，把 name/description 索引注入
//! system prompt（渐进披露第一步：只列名 + 描述，不加载 body）。
//!
//! 对应 deepagents `SkillsMiddleware`：
//! - `wrap_model_call`：首次调用 lazy 加载 index（`walkdir` 递归找 `SKILL.md`，手解析
//!   YAML frontmatter 的 `name` / `description`），随后把 `## Skills` 段 push 进
//!   `req.system_message`（与 Filesystem 等多中间件段叠加）
//! - 不引 `serde_yaml` 依赖（不改 `Cargo.toml`）：frontmatter 只取 `name:` / `description:`
//!   单行值，手解析、剥成对引号；无 frontmatter 时回退用父目录名 + body 首行
//! - defer：只做 index 注入（progressive disclosure 第一步）；不加载 skill body、不做
//!   `allowed_tools` / security cap
//!
//! # 容错
//!
//! `skills_dir` 不存在或不可读 → 注入空段（不阻断 agent）；单个 `SKILL.md` 解析失败 →
//! 跳过该条（`tracing::debug!`），其余正常注入。索引按 `name` 排序，注入顺序稳定。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use walkdir::WalkDir;

use crate::middleware::{Middleware, MiddlewareError, ModelRequest};
use crate::state::DeepAgentState;

/// 一条 skill 的元数据（从 `SKILL.md` frontmatter 解析）。
#[derive(Clone, Debug)]
pub struct SkillMetadata {
    /// Skill 名（frontmatter `name`，无 frontmatter 时用父目录名）。
    pub name: String,
    /// Skill 描述（frontmatter `description`，无 frontmatter 时用 body 首行）。
    pub description: String,
    /// `SKILL.md` 路径字符串（供后续 body 加载 defer 用）。
    pub path: String,
}

/// Skills 中间件：lazy 扫描 `skills_dir` 下 `SKILL.md`，把索引注入 system prompt。
///
/// 对应 deepagents `SkillsMiddleware`。渐进披露 MVP：只注入 name/description 列表，
/// 不加载 skill body，不做 `allowed_tools` / security cap（defer）。
pub struct SkillsMiddleware {
    skills_dir: PathBuf,
    /// `None` = 尚未加载（区别于 `Some(vec![])` = 加载了但目录空）。
    cached_index: Mutex<Option<Vec<SkillMetadata>>>,
}

impl std::fmt::Debug for SkillsMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = match self.cached_index.lock() {
            Ok(g) => g.as_ref().map_or(0, Vec::len),
            Err(_) => 0,
        };
        f.debug_struct("SkillsMiddleware")
            .field("skills_dir", &self.skills_dir)
            .field("cached_count", &count)
            .finish_non_exhaustive()
    }
}

impl SkillsMiddleware {
    /// 以给定 skills 目录构造；index 延迟到首次 `wrap_model_call` 才加载。
    #[must_use]
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            skills_dir: skills_dir.into(),
            cached_index: Mutex::new(None),
        }
    }

    /// skills 目录（只读）。
    #[must_use]
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// 强制（重新）扫描并刷新缓存，返回 skill 数量。供测试或热重载用。
    pub fn reload(&self) -> Result<usize, MiddlewareError> {
        let idx = scan_skills(&self.skills_dir)?;
        let n = idx.len();
        let mut guard = self
            .cached_index
            .lock()
            .map_err(|e| MiddlewareError::Other(format!("skills index lock poisoned: {e}")))?;
        *guard = Some(idx);
        Ok(n)
    }

    /// 取索引快照（未加载则触发加载）。返回 clone，避免持有锁。
    fn index(&self) -> Result<Vec<SkillMetadata>, MiddlewareError> {
        let mut guard = self
            .cached_index
            .lock()
            .map_err(|e| MiddlewareError::Other(format!("skills index lock poisoned: {e}")))?;
        if guard.is_none() {
            *guard = Some(scan_skills(&self.skills_dir)?);
        }
        Ok(guard.clone().unwrap_or_default())
    }
}

#[async_trait]
impl Middleware for SkillsMiddleware {
    /// 首次调用 lazy 加载 index，随后把 `## Skills` 段 push 进 `req.system_message`。
    /// 无 skill 时不注入任何内容。
    async fn wrap_model_call(
        &self,
        req: &mut ModelRequest,
        _state: &mut DeepAgentState,
    ) -> Result<(), MiddlewareError> {
        let idx = self.index()?;
        if idx.is_empty() {
            return Ok(());
        }
        let mut section = String::from("\n\n## Skills\n");
        for s in &idx {
            section.push_str(&format!("- {}: {}\n", s.name, s.description));
        }
        req.system_message.push_str(&section);
        Ok(())
    }
}

// ===== 扫描 + 解析 =====

/// 递归扫描 `root` 下名为 `SKILL.md` 的文件，解析每个的 frontmatter。
/// `root` 不存在或不可读 → 返回空 `Vec`（skills 可选，不阻断 agent）。按 `name` 排序。
fn scan_skills(root: &Path) -> Result<Vec<SkillMetadata>, MiddlewareError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name().to_str() != Some("SKILL.md") {
            continue;
        }
        let path = entry.path();
        match parse_skill_md(path) {
            Ok(meta) => out.push(meta),
            // 单文件解析失败不阻断整体扫描（对齐 deepagents 容错）。
            Err(e) => tracing::debug!("skills: skip {}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// 解析单个 `SKILL.md`：读内容 → frontmatter `name`/`description`（缺则回退到 body）。
fn parse_skill_md(path: &Path) -> Result<SkillMetadata, MiddlewareError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| MiddlewareError::Other(format!("read {}: {e}", path.display())))?;
    let path_str = path.display().to_string();
    let (fm_name, fm_desc) = extract_frontmatter(&content);
    let body = body_after_frontmatter(&content);
    let (fb_name, fb_desc) = fallback_meta(path, &body);
    // frontmatter 字段优先，缺失则用 fallback。
    let name = fm_name.unwrap_or(fb_name);
    let description = fm_desc.unwrap_or(fb_desc);
    let name = name.trim().to_string();
    let description = description.trim().to_string();
    if name.is_empty() {
        return Err(MiddlewareError::Other(format!(
            "skill md {} has empty name",
            path.display()
        )));
    }
    Ok(SkillMetadata {
        name,
        description,
        path: path_str,
    })
}

/// 取 frontmatter 之后的 body。无 frontmatter（首行非 `---`）→ 返回整个 content；
/// 有首行 `---` 但找不到闭合 `---` → 返回空串（frontmatter 畸形）。
fn body_after_frontmatter(content: &str) -> String {
    let mut lines = content.lines();
    let first = lines.next();
    match first {
        Some(l) if l.trim_start() == "---" => {
            let mut found_close = false;
            let mut body = String::new();
            for line in lines {
                if !found_close {
                    if line.trim_start() == "---" {
                        found_close = true;
                    }
                    continue;
                }
                body.push_str(line);
                body.push('\n');
            }
            // 未找到闭合 ---：frontmatter 畸形 → 空 body（fallback 用空 description）。
            if found_close { body } else { String::new() }
        }
        _ => content.to_string(),
    }
}

/// 提取 frontmatter（首行 `---` ... 次个 `---` 之间）的 `name` / `description`。
/// 返回 `(name, description)`，每项独立 `Option`：无 frontmatter 定界 → `(None, None)`；
/// 有定界但缺某字段 → 该项 `None`（由 caller 回退）。
fn extract_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    let mut lines = content.lines();
    let first = match lines.next() {
        Some(l) => l.trim_start(),
        None => return (None, None),
    };
    if first != "---" {
        return (None, None);
    }
    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if name.is_none()
            && let Some(v) = parse_kv(line, "name")
        {
            name = Some(v);
        }
        if desc.is_none()
            && let Some(v) = parse_kv(line, "description")
        {
            desc = Some(v);
        }
    }
    (name, desc)
}

/// 解析 `key: value` 行（剥成对引号）；非该 key 行返回 `None`。
/// 容忍 `key:value`（无空格）/ `"value"` / `'value'`。
fn parse_kv(line: &str, key: &str) -> Option<String> {
    let line = line.trim_start();
    // 两步剥前缀：先 key 名（`&str: Pattern`），再冒号（`char: Pattern`），避免 `&String` Pattern 依赖。
    let rest = line.strip_prefix(key)?.strip_prefix(':')?;
    let v = rest.trim();
    // 剥匹配的成对引号（ASCII 引号，strip 在 char 边界上安全）。
    let stripped = if let Some(s) = v.strip_prefix('"')
        && let Some(s) = s.strip_suffix('"')
    {
        s
    } else if let Some(s) = v.strip_prefix('\'')
        && let Some(s) = s.strip_suffix('\'')
    {
        s
    } else {
        v
    };
    Some(stripped.to_string())
}

/// 无 frontmatter（或 frontmatter 缺字段）时的回退：name = 父目录名（目录式 skill），
/// 退到文件 stem，再退到 `"skill"`；description = body 首个非空行（去 `#` 前缀）。
fn fallback_meta(path: &Path, content: &str) -> (String, String) {
    let name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "skill".to_string());
    let description = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default();
    (name, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn parses_frontmatter_with_quotes() {
        let dir = std::env::temp_dir().join("skills_test_fm");
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir.join("a").join("SKILL.md"),
            "---\nname: my-skill\ndescription: \"does a thing\"\n---\n\n# Body\n",
        );
        let m = parse_skill_md(&dir.join("a").join("SKILL.md")).unwrap();
        assert_eq!(m.name, "my-skill");
        assert_eq!(m.description, "does a thing");
    }

    #[test]
    fn parses_frontmatter_no_quotes() {
        let dir = std::env::temp_dir().join("skills_test_fmnq");
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir.join("a").join("SKILL.md"),
            "---\nname:alpha\ndescription:aaa\n---\n",
        );
        let m = parse_skill_md(&dir.join("a").join("SKILL.md")).unwrap();
        assert_eq!(m.name, "alpha");
        assert_eq!(m.description, "aaa");
    }

    #[test]
    fn fallback_when_no_frontmatter() {
        let dir = std::env::temp_dir().join("skills_test_fb");
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("plain").join("SKILL.md"), "# Plain Skill\n\nbody");
        let m = parse_skill_md(&dir.join("plain").join("SKILL.md")).unwrap();
        assert_eq!(m.name, "plain");
        assert_eq!(m.description, "Plain Skill");
    }

    #[test]
    fn frontmatter_missing_desc_uses_body_first_line() {
        let dir = std::env::temp_dir().join("skills_test_missingdesc");
        let _ = std::fs::remove_dir_all(&dir);
        // frontmatter 有 name 无 description → name 取 frontmatter，desc 回退到 body 首行。
        write(
            &dir.join("a").join("SKILL.md"),
            "---\nname: has-name\n---\n# Has Title\n\nbody",
        );
        let m = parse_skill_md(&dir.join("a").join("SKILL.md")).unwrap();
        assert_eq!(m.name, "has-name");
        assert_eq!(m.description, "Has Title");
    }

    #[test]
    fn scan_missing_dir_returns_empty() {
        let idx = scan_skills(Path::new("/no/such/skills/dir/here")).unwrap();
        assert!(idx.is_empty());
    }

    #[test]
    fn reload_and_index_sorted() {
        let dir = std::env::temp_dir().join("skills_test_reload");
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir.join("z").join("SKILL.md"),
            "---\nname: zeta\ndescription: zzz\n---\n",
        );
        write(
            &dir.join("a").join("SKILL.md"),
            "---\nname: alpha\ndescription: aaa\n---\n",
        );
        let mw = SkillsMiddleware::new(&dir);
        assert_eq!(mw.reload().unwrap(), 2);
        let idx = mw.index().unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].name, "alpha");
        assert_eq!(idx[1].name, "zeta");
    }

    #[tokio::test]
    async fn wrap_injects_skills_section() {
        let dir = std::env::temp_dir().join("skills_test_inject");
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir.join("a").join("SKILL.md"),
            "---\nname: alpha\ndescription: aaa\n---\n",
        );
        let mw = SkillsMiddleware::new(&dir);
        let mut req = ModelRequest {
            tools: vec![],
            system_message: "base".to_string(),
            options: juncture::llm::CallOptions::default(),
        };
        let mut state = DeepAgentState::default();
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        assert!(req.system_message.contains("## Skills"));
        assert!(req.system_message.contains("- alpha: aaa"));
    }

    #[tokio::test]
    async fn wrap_no_skills_no_inject() {
        let dir = std::env::temp_dir().join("skills_test_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        let mw = SkillsMiddleware::new(&dir);
        let mut req = ModelRequest {
            tools: vec![],
            system_message: "base".to_string(),
            options: juncture::llm::CallOptions::default(),
        };
        let mut state = DeepAgentState::default();
        mw.wrap_model_call(&mut req, &mut state).await.unwrap();
        assert_eq!(req.system_message, "base");
    }
}

//! Skills + Memory 注入端到端集成测试：验证 `SkillsMiddleware` + `MemoryMiddleware`
//! 在 ReAct loop 里把技能索引（`## Skills` 段，含 frontmatter name/description）
//! 与 `<agent_memory>` 段（AGENTS.md 全文）一起注入模型 invoke 收到的 system message。
//!
//! 复制自 `tests/mvp_interception.rs` 的 `SpyModel`（记录 invoke 收到的 system message）。
//! integration test 是独立 crate，helper 不能跨文件复用，故在此自包含。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deepagents::{DeepAgentBuilder, DeepAgentState, MemoryMiddleware, SkillsMiddleware};
use juncture::llm::{CallOptions, ChatModel, LlmError, Message, MessageChunk, Role, ToolDefinition};
use juncture::RunnableConfig;

/// 间谍模型：记录 bind_tools 收到的 tool 名 + invoke 收到的 system message。
#[derive(Clone)]
struct SpyModel {
    response: String,
    seen_tools: Arc<Mutex<Vec<String>>>,
    seen_system: Arc<Mutex<String>>,
}

impl SpyModel {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_system: Arc::new(Mutex::new(String::new())),
        }
    }

    #[allow(dead_code)]
    fn seen_tools(&self) -> Vec<String> {
        self.seen_tools.lock().expect("lock").clone()
    }

    fn seen_system(&self) -> String {
        self.seen_system.lock().expect("lock").clone()
    }
}

#[async_trait]
impl ChatModel for SpyModel {
    async fn invoke(
        &self,
        messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        // agent node 构造 messages 时若 system_message 非空则把 System 消息放在首位。
        // 捕获首条 System 消息的内容供断言。
        if let Some(m) = messages.first()
            && matches!(m.role, Role::System)
        {
            *self.seen_system.lock().expect("lock") = m.content_text().to_string();
        }
        // 返回无 tool_calls 的 AI 消息 → 路由 END，终止 ReAct loop。
        Ok(Message::ai(&self.response))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<juncture::llm::BoxStream<'_, Result<MessageChunk, LlmError>>, LlmError> {
        let chunk = MessageChunk {
            content: self.response.clone(),
            tool_call_chunks: Vec::new(),
            usage_delta: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    fn bind_tools(&self, tools: Vec<ToolDefinition>) -> Self {
        let mut s = self.seen_tools.lock().expect("lock");
        s.clear();
        for t in &tools {
            s.push(t.name.clone());
        }
        self.clone()
    }

    fn model_name(&self) -> &str {
        "spy"
    }
}

/// 写文件辅助：先建父目录再写。
fn write(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, content).expect("write file");
}

#[tokio::test]
async fn skills_and_memory_both_inject_into_system_message() {
    let tmp = tempfile::tempdir().expect("tmp");

    // skills_dir：放一个含 YAML frontmatter 的 SKILL.md。
    // 结构 skills_dir/my-skill/SKILL.md（walkdir 递归扫描）。
    let skills_dir = tmp.path().join("skills");
    let skill_md = skills_dir.join("my-skill").join("SKILL.md");
    write(
        &skill_md,
        "---\nname: my-skill\ndescription: \"greets the user warmly\"\n---\n\n# My Skill body\n",
    );

    // AGENTS.md：放一条有辨识度的记忆内容。
    let agents_md_path = tmp.path().join("AGENTS.md");
    let memory_marker = "Project memory: never deploy on Fridays.";
    write(&agents_md_path, &format!("# Project Notes\n\n{memory_marker}\n"));

    // SkillsMiddleware + MemoryMiddleware 同时挂载（顺序：Skills 先注入 ## Skills，
    // Memory 后注入 <agent_memory>，两段叠加进同一个 system_message）。
    let spy = SpyModel::new("done");
    let agent = DeepAgentBuilder::new(spy.clone())
        .middleware_one(SkillsMiddleware::new(skills_dir.clone()))
        .middleware_one(MemoryMiddleware::new(vec![agents_md_path.clone()]))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("hi")],
    };
    agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    let sys = spy.seen_system();

    // 断言1：system message 含 skill name（frontmatter `name`）。
    assert!(
        sys.contains("my-skill"),
        "system should contain skill name: {sys}"
    );
    // 断言2：system message 含 skill description（frontmatter `description`）。
    assert!(
        sys.contains("greets the user warmly"),
        "system should contain skill description: {sys}"
    );
    // 断言3：system message 含 Skills 段标题。
    assert!(
        sys.contains("## Skills"),
        "system should have a Skills section: {sys}"
    );
    // 断言4：system message 含 <agent_memory> 块。
    assert!(
        sys.contains("<agent_memory>"),
        "system should contain <agent_memory> block: {sys}"
    );
    assert!(
        sys.contains("</agent_memory>"),
        "system should close <agent_memory> block: {sys}"
    );
    // 断言5：system message 含 AGENTS.md 的记忆内容。
    assert!(
        sys.contains(memory_marker),
        "system should contain AGENTS.md memory content: {sys}"
    );
}

#[tokio::test]
async fn memory_middleware_skips_missing_file_but_injects_present_one() {
    // 容错：memory_sources 含一个不存在的路径 + 一个存在路径；缺失被跳过（warn），
    // 存在的正常注入。验证 MemoryMiddleware 的逐文件容错 + 仍产出 <agent_memory>。
    let tmp = tempfile::tempdir().expect("tmp");

    let missing = tmp.path().join("does-not-exist.md");
    let present = tmp.path().join("AGENTS.md");
    let marker = "remember: run tests before commit";
    write(&present, &format!("# Notes\n\n{marker}\n"));

    let spy = SpyModel::new("ok");
    let agent = DeepAgentBuilder::new(spy.clone())
        .middleware_one(MemoryMiddleware::new(vec![missing, present]))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("hi")],
    };
    agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    let sys = spy.seen_system();
    assert!(
        sys.contains("<agent_memory>"),
        "memory block present despite one missing source: {sys}"
    );
    assert!(
        sys.contains(marker),
        "present source content injected: {sys}"
    );
}

#[tokio::test]
async fn skills_empty_dir_injects_nothing() {
    // 容错：skills_dir 存在但无 SKILL.md → 不注入 ## Skills 段。
    let tmp = tempfile::tempdir().expect("tmp");
    let skills_dir = tmp.path().join("empty-skills");
    std::fs::create_dir_all(&skills_dir).expect("dir");

    let spy = SpyModel::new("done");
    let agent = DeepAgentBuilder::new(spy.clone())
        .system_prompt("base prompt")
        .middleware_one(SkillsMiddleware::new(skills_dir))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("hi")],
    };
    agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 有 base prompt → agent node push 了 System 消息 → spy 捕获到。
    // empty skills_dir → SkillsMiddleware 不追加 ## Skills 段。
    let sys = spy.seen_system();
    assert!(
        sys.contains("base prompt"),
        "base system prompt should be present: {sys}"
    );
    assert!(
        !sys.contains("## Skills"),
        "empty skills dir should not inject Skills section: {sys}"
    );
    assert!(
        !sys.contains("<agent_memory>"),
        "no memory middleware should not inject agent_memory: {sys}"
    );
}

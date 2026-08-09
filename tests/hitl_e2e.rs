#![allow(clippy::doc_lazy_continuation)] // 详尽 fact 查证 doc 含 list 续行，保留 agent 查证记录

//! HITL 端到端集成测试：`FilesystemPermission(mode=Interrupt)` 在 deepagents agent 内的行为。
//!
//! # fact 查证结论（juncture-core `interrupt/` + `pregel/loop_.rs` + `pregel/runner.rs`
//!   + `graph/compiled.rs`）
//!
//! juncture interrupt runtime API：
//! - `InterruptContext`（`interrupt/context.rs`）持 `resume_values: Arc<[Option<Value>]>`
//!   + `interrupt_tx` channel + `current_index` 原子计数。
//! - `__interrupt_impl(ctx, payload, id)`（`interrupt/mod.rs:449`）：`index=ctx.next_index()`；
//!   `ctx.get_resume_value(index)` 有值 → 返 `Ok(value)`；无值 → `ctx.send_interrupt(signal)`
//!   + 返 `Err(JunctureError::interrupted(index))`（Display = `"Interrupted at index {n}"`）。
//! - runner 在 node 执行前构造 `InterruptContext`（`runner.rs:319-323`，resume_values 来自
//!   `match_resume_to_interrupts(config.resume_value, ...)`；首次 invoke resume_value=None → 空）
//!   并 `INTERRUPT_CONTEXT.scope(ctx, ...)`（`runner.rs:485/511`）——**仅在非 inline 路径**。
//! after_tick（`loop_.rs:1187`）顺序：`apply_writes`(1221) → `compute_next_tasks`(
//!   1443, 重填 pending_tasks) → `save_superstep_checkpoint`(1566) → drain interrupt channel
//!   (1568-1592)；drain 到信号则 `LoopStatus::InterruptAfter` + `save_interrupt_checkpoint`
//!   + `finish_all_channels` + 早返 Ok。但 `tick()`(806) 顶端**不** gate `InterruptAfter`
//!   status，仅 gate `pending_tasks.is_empty()`——而 compute_next_tasks 已在 drain 前重填
//!   pending_tasks（通常含下一 agent task），故 loop 继续而非 pause。
//!
//! # 本 integration test 可達成的 TRUE E2E
//!
//! `FilesystemPermission::interrupt(Write, ["/secret/**"])` 经 `FilesystemMiddleware::
//! with_permissions` 注入 `WriteFileTool`。在 `DeepAgentBuilder` 构造的 agent 内：
//! - 工具 `check_interruptible` 命中 Interrupt 分支 → 取 task-local `InterruptContext` →
//!   `interrupt_with_ctx!` → 无论 ctx 是否设（inline 路径未 scope → try_with Err →
//!   "interrupt context not set"；非 inline 路径 scope 了 → send_signal + Err(interrupted)），
//!   工具均 catch 并返 `Ok("Error: HITL interrupt failed: ...")`，**短路 `backend.write`** →
//!   文件不创建。断言「文件未创建 + 工具结果含 HITL interrupt failed」对两条路径均成立。
//!
//! # Pregel interrupt pause + resume 完整链路 DEFER（见文件尾注释 + 返回说明）
//!
//! 1. deepagents `create_deep_agent`（`src/graph.rs:192` `checkpointer: _` + `:323`
//!    `graph.compile()`）未 wire checkpointer → `DeepAgentBuilder::checkpointer()` 当前
//!    无效 → `CompiledGraph::resume()` 必返 `"no checkpointer configured for resume"`。
//! 2. `fs_tools::check_interruptible` catch interrupt Err 后工具返 `Ok("Error: HITL...")`，
//!    ToolNode 正常完成、其 write 在 after_tick drain channel **之前** 已 merge
//!    （apply_writes:1221 先于 drain:1568）；resume 时 tools node 不会重跑（writes 已
//!    落地）→ approve/reject decision 无法触达工具，故即便接好 checkpointer，
//!    `resume("approve"→建文件 / "reject"→"rejected")` 语义亦不成立。
//! 3. 工具节点（单 task、deepagents 未配 retry/timeout/error-handler/fallback）走 runner
//!    inline fast-path（`runner.rs:99` `node.call_arc` 无 `INTERRUPT_CONTEXT.scope`）→
//!    实际命中 "interrupt context not set" 路径，interrupt signal 从未发出。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use deepagents::middleware::fs_tools::WriteFileTool;
use deepagents::{
    check_fs_permission, Backend, DeepAgentBuilder, DeepAgentState, FilesystemBackend,
    FilesystemMiddleware, FilesystemOperation, FilesystemPermission, PermissionMode,
};
use juncture::llm::{CallOptions, ChatModel, LlmError, Message, MessageChunk, Role, ToolDefinition};
use juncture::state::messages::ToolCall;
use juncture::tools::Tool;
use juncture::RunnableConfig;
use serde_json::json;

/// 脚本模型：按序返回预设 turns（content + tool_calls）。
/// 自包含复制（integration test crate 无法跨文件复用 helper）。
#[derive(Clone)]
struct ScriptedModel {
    turns: Arc<Vec<(String, Vec<ToolCall>)>>,
    index: Arc<AtomicUsize>,
}

impl ScriptedModel {
    fn new(turns: Vec<(String, Vec<ToolCall>)>) -> Self {
        Self {
            turns: Arc::new(turns),
            index: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn next_turn(&self) -> (String, Vec<ToolCall>) {
        let i = self.index.fetch_add(1, Ordering::Relaxed);
        self.turns
            .get(i)
            .or_else(|| self.turns.last())
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ChatModel for ScriptedModel {
    async fn invoke(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<Message, LlmError> {
        let (content, calls) = self.next_turn();
        Ok(Message::ai_with_tool_calls(&content, calls))
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _options: Option<&CallOptions>,
    ) -> Result<juncture::llm::BoxStream<'_, Result<MessageChunk, LlmError>>, LlmError> {
        let chunk = MessageChunk {
            content: String::new(),
            tool_call_chunks: Vec::new(),
            usage_delta: None,
        };
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }

    fn bind_tools(&self, _tools: Vec<ToolDefinition>) -> Self {
        // ScriptedModel 返回预设 turns，不依赖 bound tools 内容。
        self.clone()
    }

    fn model_name(&self) -> &str {
        "scripted"
    }
}

/// 纯逻辑：`FilesystemPermission::interrupt` 规则经 `check_fs_permission` 解析为
/// `Interrupt` / `Allow`。这是 `fs_tools::check_interruptible` 的驱动逻辑
/// （`check_interruptible` 内调 `check_fs_permission`，返回 `Interrupt` → 取 task-local
/// ctx → `interrupt_with_ctx!`）。`check_interruptible` 是私有 fn，故测其公开依赖。
#[test]
fn interrupt_permission_resolves_via_check_fs_permission() {
    let rules = vec![
        FilesystemPermission::interrupt(
            vec![FilesystemOperation::Write],
            vec!["/secret/**".into()],
        )
        .unwrap(),
    ];

    // /secret/** 命中 → Interrupt（应触发 HITL 路径，工具层视为 allow 不 deny）。
    assert_eq!(
        check_fs_permission(&rules, FilesystemOperation::Write, "/secret/x.txt"),
        PermissionMode::Interrupt,
    );
    // /public/** 不命中 → 默认 Allow（直通 backend）。
    assert_eq!(
        check_fs_permission(&rules, FilesystemOperation::Write, "/public/x.txt"),
        PermissionMode::Allow,
    );
    // Read 不在该规则 operations 内 → Allow（interrupt 仅约束 Write）。
    assert_eq!(
        check_fs_permission(&rules, FilesystemOperation::Read, "/secret/x.txt"),
        PermissionMode::Allow,
    );
}

/// 非 Pregel 上下文直接调 `WriteFileTool::invoke`（interrupt 权限）：
/// `INTERRUPT_CONTEXT` task-local 未设 → `try_with` 返 `Err(AccessError)` → 工具 catch，
/// 返 `Ok("Error: HITL interrupt failed: interrupt context not set in task-local")`，
/// 不 panic、不触达 backend（文件不创建）。验证公开 API surface（`WriteFileTool` 可直接
/// 构造 + invoke）+ catch-Err 安全性（对齐 fs_tools 单测 `interrupt_without_pregel_context_...`）。
#[tokio::test]
async fn write_file_tool_interrupt_outside_pregel_is_caught_not_panic() {
    let tmp = tempfile::tempdir().expect("tmp");
    let backend = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;
    let perms: Arc<[FilesystemPermission]> = vec![
        FilesystemPermission::interrupt(
            vec![FilesystemOperation::Write],
            vec!["/secret/**".into()],
        )
        .unwrap(),
    ]
    .into();
    let tool = WriteFileTool::new(Arc::clone(&backend), perms);

    let r = tool
        .invoke(json!({"file_path":"/secret/x.txt","content":"data"}))
        .await
        .expect("invoke returns Ok（catch Err → 不 panic、不 propagate）");

    // 含 HITL interrupt failed 前缀（工具的 catch 分支）。
    assert!(
        r.contains("HITL interrupt failed"),
        "应含 HITL interrupt failed 前缀，got: {r}"
    );
    // 非 Pregel → task-local 未设 → "context not set" 路径。
    assert!(
        r.contains("context not set"),
        "非 Pregel 应是 context not set 路径，got: {r}"
    );
    // 短路在 permission/interrupt 检查，backend.write 未触达。
    assert!(
        !tmp.path().join("secret/x.txt").exists(),
        "interrupt 未 resume 时文件不应创建"
    );
}

/// E2E（DeepAgentBuilder → Pregel 运行）：`FilesystemPermission::interrupt(Write, ["/secret/**"])`
/// 经 `FilesystemMiddleware::with_permissions` 注入 `WriteFileTool`。模型 turn1 发
/// `write_file(/secret/x.txt)` → 工具 `check_interruptible` 命中 Interrupt 分支 →
/// `interrupt_with_ctx!`（或 task-local 未设的 catch）→ 工具返
/// `Ok("Error: HITL interrupt failed: ...")` 并**短路 `backend.write`** → 文件不创建。
///
/// 断言「文件未创建 + 工具结果含 HITL interrupt failed」对 runner 的 inline 路径
/// （ INTERRUPT_CONTEXT 未 scope → "context not set"）与非 inline 路径（scope 了 →
/// "Interrupted at index 0"）均成立——两条路径均经工具的 catch-Err 返回 Ok error 串
/// 并短路 backend 调用。
///
/// 注：deepagents 工具节点为单 task 且未配 retry/timeout/error-handler/fallback，走 runner
/// inline fast-path（`runner.rs:99`，无 `INTERRUPT_CONTEXT.scope`），故实际命中
/// "context not set" 路径、interrupt signal 从未发出。真正的 Pregel pause+resume 见 defer。
#[tokio::test]
async fn interrupt_permission_blocks_write_in_deep_agent_e2e() {
    let tmp = tempfile::tempdir().expect("tmp");
    let backend = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;
    let perms = vec![
        FilesystemPermission::interrupt(
            vec![FilesystemOperation::Write],
            vec!["/secret/**".into()],
        )
        .unwrap(),
    ];

    // turn1: write_file(/secret/x.txt) —— 命中 interrupt；turn2: "done" 终止 loop。
    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: json!({"file_path":"/secret/x.txt","content":"data"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model)
        .middleware_one(FilesystemMiddleware::with_permissions(backend, perms))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("write data to /secret/x.txt")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs（工具 catch-Err 返 Ok，不 propagate Err）");

    // 断言1：文件未创建——interrupt 分支短路在 backend.write 之前。
    assert!(
        !tmp.path().join("secret/x.txt").exists(),
        "interrupt 应阻断 write，文件不应创建"
    );

    // 断言2：工具结果含 "HITL interrupt failed"——证明走了 Interrupt 分支（非 Allow 直通、
    // 非 Deny 的 "permission denied"）。具体子串（"context not set" vs "Interrupted at
    // index N"）取决于 runner 路径，不断言以保持稳健。
    let hitl_result = out.value.messages.iter().find(|m| {
        matches!(m.role, Role::Tool) && m.content_text().contains("HITL interrupt failed")
    });
    assert!(
        hitl_result.is_some(),
        "应有 HITL interrupt failed 工具结果（Interrupt 分支触发），messages: {:?}",
        out
            .value
            .messages
            .iter()
            .map(|m| (format!("{:?}", m.role), m.content_text().to_string()))
            .collect::<Vec<_>>()
    );
}

/// 对照组：同一 agent setup 但**无** interrupt 规则（默认 Allow）→ `write_file` 触达
/// backend → 文件创建 + 工具结果 "Updated file"。与上一测试对比，证明 interrupt 权限
/// 确实阻断了写（而非 backend/工具本身的问题）。
#[tokio::test]
async fn allow_permission_completes_write_control() {
    let tmp = tempfile::tempdir().expect("tmp");
    let backend = Arc::new(FilesystemBackend::new(tmp.path())) as Arc<dyn Backend>;

    let model = ScriptedModel::new(vec![
        (
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: json!({"file_path":"/public/x.txt","content":"data"}),
            }],
        ),
        ("done".into(), vec![]),
    ]);

    let agent = DeepAgentBuilder::new(model)
        .middleware_one(FilesystemMiddleware::new(backend))
        .build()
        .expect("agent builds");

    let state = DeepAgentState {
        messages: vec![Message::human("write data to /public/x.txt")],
    };
    let out = agent
        .invoke_async(state, &RunnableConfig::new())
        .await
        .expect("agent runs");

    // 文件创建（Allow → backend.write 触达）。
    assert!(
        tmp.path().join("public/x.txt").exists(),
        "Allow 应允许 write，文件应创建"
    );
    // 工具结果 "Updated file ..."（backend.write 成功）。
    let has_updated = out
        .value
        .messages
        .iter()
        .any(|m| matches!(m.role, Role::Tool) && m.content_text().contains("Updated file"));
    assert!(has_updated, "Allow 应有 Updated file 工具结果");
    // loop 正常终止：末尾有 tool-call-free 的 AI 消息 "done"。
    let last_ai = out
        .value
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::Ai) && m.tool_calls.is_empty());
    assert!(last_ai.is_some(), "Allow 路径 loop 应正常终止于 done");
}

// ===== Pregel interrupt pause + resume 完整链路 DEFER =====
//
// 目标（未達成）：`FilesystemPermission(mode=Interrupt)` 真正触发 Pregel 暂停 +
// `resume(approve)` → write_file 继续执行 → 文件创建；`resume(reject)` →
// write_file 返 "Error: rejected" + 文件不创建。
//
// 不可行的三重原因（fact 查证）：
//
// (A) checkpointer 未 wire：`create_deep_agent`（`src/graph.rs:192` `checkpointer: _`
//     忽略字段 + `:323` `graph.compile()` 而非 `compile_with_checkpointer`）。故
//     `DeepAgentBuilder::checkpointer(saver)` 当前无效果，`CompiledGraph::resume()`
//     （`compiled.rs:1236`）必返 `JunctureError::checkpoint("no checkpointer configured
//     for resume")`。修复需改 src/（不在本测试文件职责内）。
//
// (B) catch-Err 破坏 resume 重跑：`fs_tools::check_interruptible`（`src/middleware/
//     fs_tools.rs:156-168`）catch `interrupt_with_ctx!` 的 `Err(interrupted)` 后返
//     `Ok("Error: HITL interrupt failed: ...")`。ToolNode 因此正常完成、产出 tool
//     result 写入；after_tick（`loop_.rs:1221` apply_writes）在该写入 merge **之后**
//     才 drain interrupt channel（`:1568`）。resume 加载的 checkpoint 已含 tool result
//     → tools node 的 writes 已落地 → 不会重跑 → `interrupt_with_ctx!` 不会再次执行
//     → approve/reject decision 永不触达工具。即便 (A) 修好，approve→建文件 / reject
//     →"rejected" 语义亦不成立。（标准 juncture interrupt 模式要求节点 **propagate**
//     Err(interrupted) 使其不产出 writes，resume 时重跑；fs_tools 的 catch-Err 偏离
//     此模式。）
//
// (C) inline fast-path 不 scope INTERRUPT_CONTEXT：runner 单 task 且无 retry/timeout/
//     error-handler/fallback 时走 `try_execute_single_task_inline`（`runner.rs:43`），
//     其 `node.call_arc`（`:99`）**不**经 `INTERRUPT_CONTEXT.scope`——后者仅在非
//     inline 路径（`:485/511`）。deepagents 工具节点恰为单 task 且 `create_deep_agent`
//     不配 retry/timeout（`build_retry_policy_map`/`build_timeout_policy_map` 仅含显式
//     配置的节点 → 空）→ 命中 inline → INTERRUPT_CONTEXT 未 scope → try_with 返
//     AccessError → 工具命中 "context not set" 路径、interrupt signal 从未发出。
//     修复需 src/ 侧为工具节点配 retry/timeout 或改 runner（不在本测试文件职责内）。
//
// 已覆盖（可達成）：
//   - interrupt 权限经 check_fs_permission 解析为 Interrupt（纯逻辑）。
//   - WriteFileTool 非 Pregel 直接 invoke 的 catch-Err 安全性 + "context not set"。
//   - DeepAgentBuilder E2E：interrupt 权限阻断 write（文件不创建 + HITL interrupt failed
//     工具结果），与 Allow 对照组对比。
// defer：
//   - Pregel interrupt signal 真实发出 + LoopStatus::InterruptAfter pause + resume(
//     approve/reject) 触达工具决策 —— 需 src/ 侧修 (A)(B)(C) 后方可驱动。

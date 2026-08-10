# HANDOFF — deepagents-rust 复刻

## 项目
把 `D:\deepagents`（LangChain Deep Agents Python SDK）移植到 Rust，基于 `D:\juncture`（Rust LangGraph runtime）。工作目录 `D:\deepagents-rust`，分支 `feat/mvp-sdk-core`。

## 完整状态参考
- **memory 文件**（恢复全部上下文）：`C:\Users\Administrator\.claude\projects\D--deepagents-rust\memory\deepagents-rust-replication.md` — 含可行性结论、8 决策、commit 链、Phase 2a/2b/2c/2d 进度、HITL signal 链通 + juncture 上游 2 断点、全部 defer 项。**新 agent 先读此文件。**
- **commit 链**（11 commits）：`git log --oneline` — e68281a(MVP) → c083ee5(HITL signal)
- **deepagents 原版 spec 来源**：`D:\deepagents\libs\deepagents\deepagents\`（graph.py / middleware/ / backends/protocol.py）
- **juncture runtime 源码**：`D:\juncture\crates\juncture-core\src\`（pregel/runner.rs + loop_.rs + interrupt/ + graph/compiled.rs）

## 当前进度
**8 大特性全部实现**（Filesystem/Subagent/Summarization/PatchToolCalls/Skills/Memory/AsyncSubAgent/HITL）+ profiles/prompt-caching(no-op defer)/默认栈。100 测试绿 + clippy 零 warning。`examples/01_chat.rs` REPL demo 可 `cargo run`。

## 唯一未完成的关键项：HITL 完整 pause+resume
**signal 链通了**（工具 `interrupt_with_ctx!` 发 signal → after_tick drain 收到 → LoopStatus::InterruptAfter 设 → checkpoint 存 pending_interrupts），E2E 证明 tool result "Interrupted at index 0"。但完整 pause+resume 被 **2 个 juncture 上游断点**阻塞：
- **断点 A**（`D:\juncture\crates\juncture-core\src\graph\compiled.rs:658`）：`GraphOutput.interrupts` 硬编码 `Vec::new()`，从不读 `pregel.pending_interrupts()`
- **断点 B**（`D:\juncture\crates\juncture-core\src\pregel\loop_.rs:939`）：`tick()` 无 `InterruptAfter` 守卫，loop 不暂停继续跑

deepagents-rust 侧已修到极限（task-local scope + inline 工具调度 + NodeMetadata.retry_policies + propagate Err）。这 2 断点需 juncture 上游改（fork 或 PR）。

## 下一步选项
1. **向 juncture 上游报 bug**（断点 A+B，含位置 + 修复建议）— 准备 issue 描述
2. **fork juncture 改 2 断点**（`compiled.rs:658` 读 pending_interrupts + `loop_.rs:939` tick 加 InterruptAfter 守卫）— 违反 B 路径"不动 juncture"但可解 HITL
3. **接受当前状态**（HITL = signal 发 + 阻断 write，非 pause+resume），继续其他 defer 项（子 agent 继承 / 大消息驱逐 / DeltaChannel / profiles excluded_middleware 需 Middleware::name()）
4. **push 到远端**（当前 11 commits 在本地 `feat/mvp-sdk-core`，无远端）

## 验证命令
```bash
cargo test                          # 100 测试
cargo clippy --all-targets          # 零 warning
cargo run --example 01_chat         # REPL demo（无 key→MockModel / 有 key→真实 LLM）
```

## 约定
- B 路径：自建中间件层（Middleware trait + DeepAgentNode），不依赖 juncture `create_agent_with_middleware`
- 单 crate `deepagents`，path 依赖 juncture（含 juncture-core/-derive/-checkpoint）
- 行为对齐 + Rust 惯用法（不镜像 Python 签名）
- 每个 commit 后跑 `cargo test` + `cargo clippy` 零 warning

## Suggested skills
- **systematic-debugging** — 若继续深挖 juncture interrupt runtime（断点 A/B 验证）
- **git-guardrails** — 若 push / PR / branch handling
- **verify** — 若验证 HITL signal 链或真实 LLM 端到端
- **code-review** — 若 review 11 commits 的质量

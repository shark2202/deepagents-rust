# HANDOFF — deepagents-rust

## 项目

把 Python `deepagents` SDK 移植到 Rust，基于 `rig-core 0.42.0` + `rig-agent 0.42.0`（弃用 juncture，原因见 ADR-0001）。工作目录 `/Users/mac/codes/deepagents-rust`，19-crate workspace 布局。

## 完整状态参考

- **架构文档**：`docs/ARCHITECTURE.md` — 落地架构（8 模块、核心抽象、rig API 适配备忘）
- **SPEC survey**：`docs/SPEC.md`（961 行，Q1-Q26 全量勘测）
- **ADR**：`docs/adr/` 4 篇决策记录
- **commit 链**：`git log --oneline` — 3 commits
  - `3783c3e` workspace skeleton (19 crates)
  - `a57bdba` deepagents-errors (~40 错误类型)
  - `59d32ff` deepagents-core (8 模块, 51 tests + 1 doctest)
- **rig 源码**：`~/.cargo/registry/src/rsproxy.cn-e3de039b2554c837/rig-core-0.42.0/` 和 `rig-agent-0.42.0/`

## 已完成

### deepagents-errors (Q26)
- ~40 错误类型，7 个域（Io/Config/Sandbox/Agent/Session/Tool/Provider）
- `Error` enum + `miette::Diagnostic` + `StartupErrorMarker`
- ✅ 提交 `a57bdba`

### deepagents-core (Q4-Q8)
8 个模块，51 单元测试 + 1 doctest，零警告：

| 模块 | 文件 | 职责 | SPEC |
|------|------|------|------|
| `backend` | `backend.rs` | Backend trait + 4 backends (State/Filesystem/LocalShell/Composite) | Q7 |
| `permission` | `permission.rs` | FilesystemPermission + PermissionChecker (glob 匹配) | Q7 |
| `mock` | `mock.rs` | MockCompletionModel (FIFO 响应队列) | Q8 |
| `hitl` | `hitl.rs` | InterruptPolicy + InterruptMap + HitlMiddleware (pause/resume) | Q4 |
| `subagent` | `subagent.rs` | SubAgentSpec / HarnessProfile / ModelSpec / McpServerConfig | Q6 |
| `middleware` | `middleware.rs` | Filesystem/SubAgent/Summarization 三件套 | Q5 |
| `builder` | `builder.rs` | DeepAgentBuilder 18-param + HookStack | Q6 |
| `lib` | `lib.rs` | 模块声明 + re-exports | — |

✅ 提交 `59d32ff`

### 文档
- `docs/ARCHITECTURE.md` — 落地架构文档
- `docs/adr/0001-rig-as-base.md` — 选择 rig（弃 juncture）
- `docs/adr/0002-hookstack-over-dyn-agenthook.md` — HookStack 包装方案
- `docs/adr/0003-builder-generic-placeholder-model.md` — 泛型 builder + 默认占位 model
- `docs/adr/0004-middleware-impl-agenthook-directly.md` — 中间件直接 impl AgentHook

## 关键设计决策

| 决策 | 原因 | ADR |
|------|------|-----|
| rig 而非 juncture | juncture 有 2 个上游断点（HITL + run-loop） | 0001 |
| `HookStack` 而非 `Arc<dyn AgentHook>` | `AgentHook` 不 dyn-compatible | 0002 |
| `DeepAgentBuilder<M = MockModelPlaceholder>` | `CompletionModel` 方法返回 `impl Future`，不 dyn-compatible | 0003 |
| 中间件直接 `impl AgentHook` | 无需包装 trait，与 rig API 直接对齐 | 0004 |

### rig API 适配备忘
- `Agent.config` 字段私有 — 测试只能用 `agent.name()` 等公开方法
- `Message` 不实现 `Display` — 用 `rag_text() -> Option<String>` 提取文本
- `Message::from_text()` 不存在 — 用 `Message::user(text)` / `Message::assistant(text)`
- `HookStack::push()` 只有 append（无 push_front）
- `RequestPatch::new().preamble(s).active_tools(names).history(msgs)` — 链式 patch
- `AgentRunner::from_agent(&agent, prompt)` 需要 `&Agent` 引用

## 验证命令

```bash
cargo check -p deepagents-core        # 零警告编译
cargo test -p deepagents-core          # 51 tests + 1 doctest
cargo check --workspace                # 全 workspace 编译（骨架 crates）
```

## 下一步

剩余 17 个 crate 为骨架状态（`lib.rs` 空声明 + `Cargo.toml` 依赖声明）。下一批优先级：

1. `deepagents-config` — config.toml 6-layer resolver (Q11)
2. `deepagents-sessions` — rusqlite checkpoint/resume (Q17)
3. `deepagents-sandbox` — trait provider, 6 providers (Q21)
4. `deepagents-cli` — clap, 13 subcommands (Q13)
5. `deepagents-tui` — ratatui TUI (Q14, Q24)

## 约定

- `#![forbid(unsafe_code)]` + `#![warn(missing_docs)]` 全 crate 生效
- 零警告策略
- 行为对齐 + Rust 惯用法（不镜像 Python 签名）
- 所有架构决策映射到 SPEC.md Q-numbers (Q1-Q26)
- 每个 commit 后跑 `cargo test` + `cargo check` 零警告

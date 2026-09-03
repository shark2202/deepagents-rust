# ADR-0001: 选择 rig 作为底座（弃用 juncture）

- **状态**: Accepted
- **日期**: 2026-09-01
- **决策者**: deepagents-rust 团队
- **关联 SPEC**: Q3（底座选型）

## 上下文

deepagents-rust 需要一个 Rust agent SDK 作为底座，提供：

1. Agent run-loop state machine
2. Tool 调用生命周期 hook（`on_tool_call`, `on_tool_result`, `on_completion_call` 等）
3. Completion model 抽象（多 provider 支持）
4. 消息历史管理 + summarization
5. 可序列化的 run state（checkpoint/resume）

### 候选方案

#### 方案 A：juncture

`juncture` 是一个 Rust LangGraph 移植，理论上最接近 Python deepagents 的 LangGraph 基底。

**问题**：在评估期间发现 2 个上游断点（blocking issues）：

1. `compiled.rs` 中的状态编译逻辑断点 — 导致 HITL pause/resume 无法正常工作
2. `loop_.rs` 中的循环执行逻辑断点 — 导致 agent run-loop 无法稳定迭代

这两个断点直接命中 deepagents 的核心需求（HITL 中断 + run-loop），无法绕过。

#### 方案 B：rig (rig-core + rig-agent)

`rig` 是一个成熟的 Rust LLM framework，提供：

- `Agent` struct + `AgentBuilder` fluent API
- `AgentRun` sans-IO state machine
- `AgentHook` trait（生命周期 hook）
- `HookStack`（多 hook 聚合）
- `CompletionModel` trait + 多 provider 实现
- `RequestPatch`（preamble / active_tools / history 运行时修改）
- `ToolCallAction::{Run, skip, stop}` — 可拦截 tool 调用

**优势**：
- 正在维护，版本 0.42.0，API 稳定
- `AgentHook` 提供我们需要的所有生命周期事件
- `RequestPatch` 可实现 preamble 注入和 tool 过滤
- `ToolCallAction::Stop` 可实现 HITL pause

**劣势**：
- `AgentHook` 不 dyn-compatible（见 ADR-002）
- `CompletionModel` 的方法返回 `impl Future`，需泛型 builder（见 ADR-003）
- 不是 LangGraph 移植，状态机语义与 Python 版有差异

## 决策

**选择 rig (rig-core 0.42.0 + rig-agent 0.42.0) 作为底座。**

juncture 的 2 个上游断点直接命中 HITL + run-loop 两个核心需求，无法接受。rig 虽然有 dyn-compatibility 和泛型约束，但这些是可工程化解决的问题（见 ADR-002, ADR-003），而非上游不可修复的断点。

## 后果

### 正面

- ✅ 立即可用：AgentRun + AgentHook + HookStack 全部可用
- ✅ HITL 通过 `ToolCallAction::Stop` 实现，语义清晰
- ✅ `RequestPatch` 实现 preamble 注入和 tool 过滤
- ✅ 多 provider 支持开箱即用
- ✅ 活跃维护，版本可追踪

### 负面

- ⚠️ `AgentHook` 不 dyn-compatible → 需 `HookStack` 包装（ADR-002）
- ⚠️ `CompletionModel` 方法返回 `impl Future` → builder 需泛型参数（ADR-003）
- ⚠️ `Agent.config` 字段私有 → 测试只能用公开方法

### 中性

- 状态机语义与 Python LangGraph 不同，但 sans-IO 设计更适合 Rust
- `AgentRun` 的 checkpoint/resume 需手动实现（rig 不提供 checkpointer）

## 验证

- `deepagents-core` 的 51 个单元测试 + 1 个 doctest 全部通过
- `HitlMiddleware` 通过 `ToolCallAction::Stop` 成功实现 pause
- `FilesystemMiddleware` 通过 `RequestPatch` 成功注入 preamble
- `SummarizationMiddleware` 通过 `RequestPatch` 成功截断历史

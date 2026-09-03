# ADR-0002: 使用 HookStack 包装而非 `Arc<dyn AgentHook>`

- **状态**: Accepted
- **日期**: 2026-09-01
- **决策者**: deepagents-rust 团队
- **关联 SPEC**: Q5（中间件架构）

## 上下文

deepagents 需要在 agent run-loop 的生命周期事件中注入多个 hook（FilesystemMiddleware、SubAgentMiddleware、SummarizationMiddleware、HitlMiddleware + 用户自定义 hook）。rig-agent 提供了 `AgentHook` trait 用于生命周期拦截。

### 问题

`AgentHook` trait **不 dyn-compatible**（旧称 object-safe）。原因：

```rust
pub trait AgentHook {
    fn on_tool_call(&mut self, event: ToolCallEvent) -> ToolCallAction { ... }
    fn on_tool_result(&mut self, event: ToolResultEvent) { ... }
    fn on_completion_call(&mut self, event: CompletionCallEvent) -> RequestPatch { ... }
    // ...其他生命周期事件
}
```

虽然每个方法看起来可以 object-safe，但 trait 中包含了返回 `impl Future` 的方法签名（如 `on_run_step` 等），这些方法签名使用 `Self`，导致 trait 不 dyn-compatible。

因此无法使用：
```rust
let hooks: Vec<Arc<dyn AgentHook>> = vec![...];  // ❌ 编译失败
```

### 候选方案

#### 方案 A：rig 自带的 `HookStack`

rig-agent 提供 `HookStack` struct，内部用 `Vec<Box<dyn Any>>` 存储 hook，在事件发生时 downcast 并调用。

```rust
let mut stack = HookStack::new();
stack.push(FilesystemMiddleware::new(backend, perms));
stack.push(HitlMiddleware::new(interrupt_map));
agent_builder.add_hook(stack);
```

**优势**：
- rig 原生支持，无需自己实现
- 类型安全：`HookStack::push<H: AgentHook>(hook)` 在编译时检查类型
- 与 `AgentBuilder::add_hook()` 直接兼容

**劣势**：
- `push()` 只有 append（无 `push_front`），hook 执行顺序是追加序
- 内部用 `Box<dyn Any>` + downcast，有轻微运行时开销

#### 方案 B：自定义 enum 分发

```rust
enum MiddlewareHook {
    Filesystem(FilesystemMiddleware),
    SubAgent(SubAgentMiddleware),
    Summarization(SummarizationMiddleware),
    Hitl(HitlMiddleware),
    Custom(Box<dyn AgentHook>),  // 仍不可行
}
```

**问题**：
- 枚举变体固定，用户无法在不修改 enum 的情况下添加自定义中间件
- `Custom` 变体仍需 dyn dispatch，回到 object-safety 问题
- 违反 open-closed 原则

#### 方案 C：泛型 hook 列表

```rust
struct DeepAgent<H1: AgentHook, H2: AgentHook, ...> { ... }
```

**问题**：组合爆炸（N 个 hook → N! 个泛型参数），完全不可行。

## 决策

**使用 rig 原生的 `HookStack`。**

`HookStack` 是 rig-agent 提供的现成方案，类型安全、无 dyn-compatibility 问题、与 `AgentBuilder` 直接兼容。运行时 downcast 开销可忽略（hook 调用频率远低于 LLM 调用）。

## 实现

在 `builder.rs` 中：

```rust
let mut hook_stack = HookStack::new();

// 标准中间件按固定顺序追加
if !profile.is_middleware_excluded("Filesystem") {
    hook_stack.push(FilesystemMiddleware::new(backend.clone(), perms.clone()));
}
if !profile.is_middleware_excluded("SubAgent") {
    hook_stack.push(SubAgentMiddleware::new(subagents.clone()));
}
if !profile.is_middleware_excluded("Summarization") {
    hook_stack.push(SummarizationMiddleware::new(max_iterations));
}
if !profile.is_middleware_excluded("Hitl") {
    hook_stack.push(HitlMiddleware::new(interrupt_map.clone()));
}

// 用户自定义 hook 追加在标准中间件之后
for hook in self.extra_hooks {
    hook_stack.push(hook);
}

agent_builder.add_hook(hook_stack);
```

### Hook 执行顺序

由于 `HookStack::push()` 是 append，执行顺序为追加顺序：
1. FilesystemMiddleware
2. SubAgentMiddleware
3. SummarizationMiddleware
4. HitlMiddleware
5. [用户自定义 hook...]

`Filesystem` 和 `SubAgent` 受保护，不可通过 `HarnessProfile::exclude_middleware()` 排除。

## 后果

### 正面

- ✅ 无 dyn-compatibility 问题，编译通过
- ✅ 类型安全：`push<H: AgentHook>` 在编译时检查
- ✅ 与 rig `AgentBuilder` 零适配开销
- ✅ 用户可添加任意数量的自定义 hook

### 负面

- ⚠️ hook 执行顺序固定为追加序，无法在标准中间件之前插入自定义 hook
- ⚠️ `HookStack` 内部 `Box<dyn Any>` + downcast 有极轻微运行时开销

## 备注

rig 的 `HookStack` 在 0.42.0 中没有 `push_front` 或优先级参数。如果未来需要更灵活的 hook 排序，可考虑：
1. 提交 PR 到 rig 上游添加 `push_front` / `insert`
2. 在 deepagents 层面包装 `HookStack`，添加优先级排序

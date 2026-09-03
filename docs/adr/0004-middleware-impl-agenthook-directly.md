# ADR-0004: 中间件直接 `impl AgentHook`，不引入包装 trait

- **状态**: Accepted
- **日期**: 2026-09-01
- **决策者**: deepagents-rust 团队
- **关联 SPEC**: Q5（中间件架构）

## 上下文

deepagents 有三个标准中间件（FilesystemMiddleware、SubAgentMiddleware、SummarizationMiddleware）加上一个 HITL 中间件（HitlMiddleware），都需要在 agent run-loop 的生命周期事件中拦截。

rig-agent 提供了 `AgentHook` trait，每个中间件需要实现这个 trait 的方法来拦截对应的生命周期事件。

## 问题

设计时面临一个问题：是否需要一个中间层 trait（如 `Middleware` trait）来包装 `AgentHook`？

### 候选方案

#### 方案 A：引入 `Middleware` 包装 trait

```rust
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str;
    // 转发到 AgentHook 方法，或提供额外中间件特有 API
    fn as_hook(&self) -> &dyn AgentHook;  // ❌ AgentHook 不 dyn-compatible
}
```

然后：
```rust
pub struct FilesystemMiddleware { ... }
impl Middleware for FilesystemMiddleware { ... }
impl AgentHook for FilesystemMiddleware { ... }  // 实际生命周期逻辑
```

**问题**：
- `AgentHook` 不 dyn-compatible（见 ADR-002），`as_hook(&self) -> &dyn AgentHook` 编译失败
- 即使去掉 `as_hook`，`Middleware` trait 变成纯标记 trait，无实际运行时价值
- 增加了一层间接性，用户需理解两个 trait 的关系
- 每个 hook 方法需在 `Middleware` 和 `AgentHook` 两处维护

#### 方案 B：直接 `impl AgentHook`

每个中间件 struct 直接实现 `AgentHook` trait：

```rust
pub struct FilesystemMiddleware { ... }

impl AgentHook for FilesystemMiddleware {
    fn on_completion_call(&mut self, event: CompletionCallEvent) -> RequestPatch {
        // 注入 fs tools 到 active_tools
        // 注入 fs preamble 到 preamble
        let mut patch = RequestPatch::new();
        patch = patch.active_tools(self.fs_tool_names());
        patch = patch.preamble(self.fs_preamble());
        patch
    }

    fn on_tool_result(&mut self, event: ToolResultEvent) {
        // eviction logic: 如果结果太大，截断
    }
}
```

**优势**：
- 无额外 trait，零间接性
- 与 rig API 直接对齐（`HookStack::push<H: AgentHook>(hook)`）
- 用户自定义 hook 也直接 `impl AgentHook`，API 一致
- 每个 hook 逻辑集中在一个 `impl` 块中

**劣势**：
- 中间件没有统一的 "name" 方法（但 `AgentHook` 本身不要求 name）
- 不能通过 trait object 分发中间件特有的 API（如 `FilesystemMiddleware::set_eviction_limit()`）

#### 方案 C：泛型 trait + blanket impl

```rust
pub trait Middleware: AgentHook {
    fn name(&self) -> &str;
}

// blanket impl — 任何 AgentHook 都是 Middleware
impl<H: AgentHook> Middleware for H {
    fn name(&self) -> &str { "hook" }
}
```

**问题**：
- blanket impl 使所有 `AgentHook` 都自动成为 `Middleware`，`name()` 返回固定值无意义
- 仍不解决 dyn-compatibility 问题
- 过度工程化

## 决策

**每个中间件 struct 直接 `impl AgentHook`，不引入 `Middleware` 包装 trait。**

理由：
1. `AgentHook` 不 dyn-compatible，包装 trait 无法提供 dyn 分发能力
2. 中间件特有的 API（如 `FilesystemMiddleware::set_eviction_limit()`）可通过具体类型直接调用，不需 trait 抽象
3. `HookStack::push<H: AgentHook>()` 直接接受具体类型，无需中间层
4. API 一致性：标准中间件和用户自定义 hook 使用相同的 `impl AgentHook` 模式

## 实现

### 标准三中间件（middleware.rs）

```rust
// FilesystemMiddleware — 注入 fs tools + preamble，执行 eviction
impl AgentHook for FilesystemMiddleware {
    fn on_completion_call(&mut self, event: CompletionCallEvent) -> RequestPatch {
        let mut patch = RequestPatch::new();
        patch = patch.active_tools(self.fs_tool_names());
        patch = patch.preamble(self.fs_preamble());
        patch
    }
    fn on_tool_result(&mut self, event: ToolResultEvent) {
        // eviction: 如果 tool result 超过阈值，截断或标记
    }
}

// SubAgentMiddleware — 注入 task tool + subagent 描述 preamble
impl AgentHook for SubAgentMiddleware {
    fn on_completion_call(&mut self, event: CompletionCallEvent) -> RequestPatch {
        let mut patch = RequestPatch::new();
        patch = patch.active_tools(vec!["task".to_string()]);
        patch = patch.preamble(self.subagent_descriptions());
        patch
    }
}

// SummarizationMiddleware — 截断过长的历史消息
impl AgentHook for SummarizationMiddleware {
    fn on_completion_call(&mut self, event: CompletionCallEvent) -> RequestPatch {
        let mut patch = RequestPatch::new();
        if self.history_too_long() {
            patch = patch.history(self.summarized_history());
        }
        patch
    }
    fn on_tool_result(&mut self, event: ToolResultEvent) {
        // 截断过大的 tool result
    }
}
```

### HITL 中间件（hitl.rs）

```rust
// HitlMiddleware — 拦截 tool call，匹配 interrupt_on 规则
impl AgentHook for HitlMiddleware {
    fn on_tool_call(&mut self, event: ToolCallEvent) -> ToolCallAction {
        match self.interrupt_map.check(&event.tool_call_info) {
            ApprovalDecision::Approve => ToolCallAction::Run,
            ApprovalDecision::Reject(reason) => ToolCallAction::skip(reason),
            ApprovalDecision::RequireHuman => ToolCallAction::stop("awaiting approval"),
        }
    }
}
```

### 用户自定义 hook

用户直接 `impl AgentHook` 即可，与标准中间件使用相同模式：

```rust
struct MyCustomHook;

impl AgentHook for MyCustomHook {
    fn on_tool_call(&mut self, event: ToolCallEvent) -> ToolCallAction {
        // 自定义逻辑
        ToolCallAction::Run
    }
}

// 在 builder 中
builder.hook::<MyCustomHook>(MyCustomHook);
```

## 后果

### 正面

- ✅ 零间接性：中间件逻辑直接在 `impl AgentHook` 块中
- ✅ API 一致性：标准中间件和用户自定义 hook 使用相同模式
- ✅ 与 rig `HookStack::push<H: AgentHook>()` 直接兼容
- ✅ 无需维护额外 trait 和 blanket impl
- ✅ 中间件特有的配置 API 通过具体类型直接调用

### 负面

- ⚠️ 中间件没有统一的 `name()` 方法 — 但 `AgentHook` 本身不要求 name，且 `HarnessProfile::is_middleware_excluded()` 用字符串匹配（`"Filesystem"`、`"SubAgent"`），由 builder 在组装时硬编码判断，不依赖 trait 方法
- ⚠️ 无法通过 trait object 集中管理中间件 — 但 `HookStack` 已经提供了聚合能力

## 备注

`HarnessProfile::is_middleware_excluded()` 的受保护机制（`"Filesystem"` 和 `"SubAgent"` 不可排除）是在 builder 组装时通过字符串匹配实现的，不依赖中间件 struct 的 `name()` 方法。这避免了为每个中间件实现 `name()` 的需要。

```rust
// builder.rs 中的排除逻辑
if !profile.is_middleware_excluded("Filesystem") {
    hook_stack.push(FilesystemMiddleware::new(...));
}
```

这种设计将排除逻辑集中在 builder 中，中间件 struct 本身不需要知道自己是否"可排除"。

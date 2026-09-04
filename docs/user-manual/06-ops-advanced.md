# deepagents 运维手册 · 高级

> **适用对象**：需要深入理解架构、crate 依赖图、做高级排障和性能调优的资深运维/架构师。
>
> **前置条件**：已读完 [运维初级](./04-ops-beginner.md) 和 [运维中级](./05-ops-intermediate.md) 手册。

---

## 一、Workspace 架构总览

deepagents-rust 是一个 Cargo workspace，包含 **20 个 crate**（1 个顶层库 + 19 个子 crate）：

```
deepagents-rust/
├── Cargo.toml                    # workspace 根
├── crates/
│   ├── deepagents/               # 顶层 umbrella crate，re-export 全部子 crate
│   ├── deepagents-core/          # DeepAgentBuilder, 核心抽象
│   ├── deepagents-runtime/       # ★ Provider 解析 + LLM 接线（已实现）
│   ├── deepagents-cli/           # ★ CLI 参数解析 + main.rs 二进制（已实现）
│   ├── deepagents-config/        # 配置系统骨架
│   ├── deepagents-env/           # ★ 55 env vars, dotenvy, 3-layer denylist
│   ├── deepagents-tui/           # TUI 交互界面骨架
│   ├── deepagents-hooks/         # Hooks 系统骨架
│   ├── deepagents-plugins/       # 插件系统骨架
│   ├── deepagents-sessions/      # 会话管理骨架
│   ├── deepagents-approval/      # 审批流骨架
│   ├── deepagents-mcp/           # MCP 服务器骨架
│   ├── deepagents-skills/        # 技能系统骨架
│   ├── deepagents-sandbox/       # 沙箱执行骨架
│   ├── deepagents-cost/          # 成本追踪骨架
│   ├── deepagents-goal/          # Goal/Issue 循环骨架
│   ├── deepagents-onboarding/    # 引导流程骨架
│   ├── deepagents-update/        # 自更新骨架
│   ├── deepagents-doctor/        # 诊断工具骨架
│   └── deepagents-errors/        # 统一错误类型
└── docs/
    └── user-manual/              # 本手册
```

> ★ = 当前已实现核心功能的 crate

---

## 二、Crate 依赖图

```
                    ┌──────────────┐
                    │ deepagents   │  (umbrella, re-exports all)
                    └──────┬───────┘
                           │
          ┌────────────────┼────────────────────────┐
          │                │                        │
    ┌─────▼─────┐   ┌─────▼──────┐          ┌──────▼──────┐
    │  cli ★    │   │  core      │          │  errors     │
    │ (binary)  │   │ (builder)  │          │ (Error/     │
    └─────┬─────┘   └─────┬──────┘          │  ConfigErr) │
          │                │                 └──────┬──────┘
          │           ┌────┴───────────────────────┘
          │           │
    ┌─────▼─────┐ ┌───▼──────┐ ┌──────┐ ┌────────┐
    │ runtime ★ │ │ config   │ │ env  │ │ errors │
    │ (provider │ │ (toml)  │ │(dotenvy)│        │
    │  dispatch)│ └──────────┘ └──────┘ └────────┘
    └─────┬─────┘
          │
    ┌─────▼──────────────────────┐
    │ rig-core 0.42 + rig-agent │
    │ (OpenAI/Anthropic/Ollama   │
    │  provider clients)         │
    └────────────────────────────┘
```

### 依赖链路（关键路径）

```
main.rs (cli)
  → deepagents_cli::CliRunner
  → deepagents_env::EnvRegistry::load_dotenv_default()
  → deepagents_runtime::ProviderModel::from_env_default()
    → deepagents_runtime::ProviderModel::new(provider, model)
      → rig_core::providers::{openai,anthropic,ollama}::Client::from_env()
      → client.completion_model(model_name)
  → deepagents_runtime::run_prompt_with(model, system_prompt, prompt)
    → deepagents_core::builder::DeepAgentBuilder::new()
      → .model(model)
      → .system_prompt(...)
      → .build_runner(prompt)
    → rig_agent::agent::AgentRunner::run().await
    → PromptResponse { output: String }
```

### Cargo 依赖关系

| crate | 依赖 |
|-------|------|
| `deepagents-cli` | `deepagents-cli`(lib) + `deepagents-runtime` + `deepagents-env` + `tokio` |
| `deepagents-runtime` | `deepagents-core` + `rig-core` + `rig-agent` + `tokio` + `thiserror` |
| `deepagents-core` | `rig-core` + `rig-agent` + `deepagents-errors` |
| `deepagents-env` | `dotenvy` + `deepagents-errors` + `serde` |
| `deepagents-errors` | `thiserror` |

---

## 三、关键技术决策

### 3.1 Enum Dispatch vs Trait Object

**问题**：rig 的 `CompletionModel` trait 使用了 RPITIT（return-position `impl Trait` in trait），不是 object-safe 的，无法 `dyn CompletionModel`。

**解决方案**：`ProviderModel` 枚举封装 + match 分发：

```rust
pub enum ProviderModel {
    Openai(openai::CompletionModel),
    Anthropic(anthropic::completion::CompletionModel),
    Ollama(ollama::CompletionModel),
}

impl CompletionModel for ProviderModel {
    fn completion(&self, req: CompletionRequest)
        -> impl Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend
    {
        async move {
            match self {
                Self::Openai(m) => m.completion(req).await,
                Self::Anthropic(m) => m.completion(req).await,
                Self::Ollama(m) => m.completion(req).await,
            }
        }
    }
}
```

**影响**：新增 provider 需修改 enum + 3 处 match arm。不如 trait object 灵活，但类型安全且零开销。

### 3.2 OpenAI Completions API vs Responses API

**问题**：rig 的 `openai::Client` 默认用 Responses API，`client.completion_model()` 返回 `GenericResponsesCompletionModel`，与 `CompletionModel` 类型别名不匹配。

**解决方案**：切到 Chat Completions API：

```rust
let client = openai::Client::from_env()?;
let completions_client = client.completions_api();  // 关键
Ok(ProviderModel::Openai(completions_client.completion_model(model)))
```

### 3.3 Tokio 单线程 Runtime

```rust
let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;
```

**原因**：deepagents 的 agent 循环是顺序的（无并行工具调用），单线程 runtime 足够且更轻量。如果未来支持并行工具执行，需切换到 `new_multi_thread()`。

### 3.4 `.env` 非 Override 模式

```rust
// 非 override：Shell 环境变量优先，.env 只填补空缺
dotenvy::from_path(path)?;           // 当前选择
// override：.env 覆盖一切
// dotenvy::from_path_override(path)?;
```

**影响**：CI 环境通过 secret 注入的变量不会被项目 `.env` 覆盖，符合 12-Factor App 规范。

### 3.5 Anthropic 路径

```rust
// ✅ 正确：completion 子模块
use rig_core::providers::anthropic::completion::CompletionModel;

// ❌ 错误：provider 根级不 re-export completion 子模块
// use rig_core::providers::anthropic::CompletionModel;  // 编译失败
```

---

## 四、构建配置详解

### 4.1 Workspace `[workspace.package]`

```toml
[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.88"
license = "Apache-2.0"
```

所有子 crate 继承这些字段：`version.workspace = true`。

### 4.2 Lint 规则

每个 crate 都有：

```rust
#![forbid(unsafe_code)]   // 禁止 unsafe
#![warn(missing_docs)]    // 公开 item 必须有文档
```

**影响**：所有 `pub` 函数/类型/变体必须有 `///` 文档注释，否则编译警告。

### 4.3 Profile 配置

默认 profile（无自定义）：

```toml
# [profile.release]  # 当前未设自定义 profile
# opt-level = 3
# lto = true
# strip = true
```

如需更小二进制，可在 workspace 根 `Cargo.toml` 加：

```toml
[profile.release]
opt-level = "z"      # 优化体积
lto = true           # 链接时优化
strip = true         # 去除调试符号
codegen-units = 1    # 单线程编译（更慢，更小）
```

---

## 五、高级排障

### 5.1 编译时排障

#### 问题：某个 crate 缺少 doc comment

```
warning: missing documentation for a variant
```

**原因**：`#![warn(missing_docs)]` 要求所有 pub item 有文档。

**解决**：给缺少文档的 pub item 加 `///` 注释。

#### 问题：`unsafe_code` forbidden

```
error: unsafe code is forbidden
```

**原因**：`#![forbid(unsafe_code)]` 禁止所有 unsafe。

**解决**：不使用 unsafe。如果依赖（如 rusqlite 内部）使用 unsafe，这没问题——forbid 只影响本 crate 源码。

### 5.2 运行时排障

#### 问题：Provider 解析到错误的 provider

**排查**：打印实际解析结果：

```bash
# 临时用 --model 显式指定，绕过自动推断
deepagents --model "openai:gpt-4o" -p "hello"

# 检查环境变量实际值
env | grep -E "OPENAI|ANTHROPIC|OLLAMA|DEEPAGENTS"
```

#### 问题：`.env` 不生效

**排查步骤**：

```bash
# 1. 确认 .env 在当前工作目录
ls -la .env

# 2. 确认文件格式正确（等号两边无空格）
cat -A .env  # -A 显示隐藏字符，检查有无 BOM/CR

# 3. 确认没有 shell 变量覆盖
# Shell 变量 > .env，如果 shell 已设 KEY，.env 的值会被忽略
env | grep OPENAI_API_KEY  # 如果有输出，说明 shell 已设

# 4. 用 strace/dtruss 确认 deepagents 读取了 .env
# macOS:
sudo dtruss -f deepagents -p "hello" 2>&1 | grep .env
# Linux:
strace -e trace=openat deepagents -p "hello" 2>&1 | grep .env
```

#### 问题：SSL/TLS 错误

```
error sending request for url (https://...): certificate verification failed
```

**原因**：项目用 `rustls-tls`（纯 Rust TLS），不读系统 CA 证书。

**解决**：
```bash
# 检查系统时间（TLS 依赖正确时间）
date

# 如果是自签名证书的 Ollama，确保 OLLAMA_API_BASE_URL 用 http:// 而非 https://
```

### 5.3 网络层排障

#### 抓包确认请求目标

```bash
# 用 --debug + tcpdump 确认实际请求 URL
deepagents --debug -p "hello" 2>&1 | grep -i "url\|request\|connect"

# macOS 抓包
sudo tcpdump -i any -A 'host api.openai.com and port 443' &

# Linux 抓包
sudo tcpdump -i any -A 'host token.bytebroad.com.cn and port 443' &
```

#### 确认 rig 实际读的环境变量

```bash
# rig openai client 读 OPENAI_BASE_URL（不是 OPENAI_API_BASE_URL！）
env | grep OPENAI_BASE_URL
```

> 这是最高频的坑：bbgate/第三方 API 场景必须设 `OPENAI_BASE_URL`，而非 `OPENAI_API_BASE_URL`。

---

## 六、性能调优

### 6.1 编译速度

```bash
# 使用 sccache 加速重复编译
cargo install sccache
export RUSTC_WRAPPER=sccache
cargo build --release -p deepagents-cli

# 使用 lld 链接器（Linux）
export RUSTFLAGS="-C link-arg=-fuse-ld=lld"
cargo build --release -p deepagents-cli

# 使用 mold 链接器（Linux，更快）
export RUSTFLAGS="-C link-arg=-fuse-ld=mold"
```

### 6.2 二进制体积

```bash
# 默认 release 体积
ls -lh target/release/deepagents
# 约 15-25 MB

# 加 strip profile
# [profile.release]
# strip = true
# → 约 8-12 MB

# musl 静态链接 + strip
cargo build --release -p deepagents-cli --target x86_64-unknown-linux-musl
ls -lh target/x86_64-unknown-linux-musl/release/deepagents
# 约 8-15 MB
```

### 6.3 运行时性能

deepagents 的运行时瓶颈在 **LLM API 网络延迟**，不在本地计算。

| 阶段 | 典型耗时 | 优化方向 |
|------|---------|---------|
| `.env` 加载 | <1ms | 无需优化 |
| Provider client 构造 | <5ms | 无需优化 |
| AgentRunner 循环 | 取决于 LLM | 选择更快的模型 |
| HTTP 请求 + LLM 生成 | 500ms-10s | 网络/模型选型 |

**模型选择建议**（按延迟从低到高）：

| 模型 | 典型首 token 延迟 | 说明 |
|------|------------------|------|
| Ollama llama3.2 (本地) | 100-500ms | 无网络延迟 |
| OpenAI gpt-4o-mini | 300-800ms | 适合简单任务 |
| bbgate glm-5.2 | 500-1500ms | 国内访问稳定 |
| Anthropic claude-sonnet | 800-2000ms | 高质量推理 |
| OpenAI gpt-4o | 500-1500ms | 综合能力强 |

### 6.4 Tokio Runtime 调优

当前使用单线程 runtime。如果需要并行（未来多工具并发）：

```rust
// 切换到多线程
let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .enable_all()
    .build()?;
```

---

## 七、CI/CD 集成

### 7.1 GitHub Actions

```yaml
# .github/workflows/build.yml
name: Build & Test
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - name: Check formatting
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Test
        run: cargo test --workspace

  build-release:
    needs: test
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build
        run: cargo build --release -p deepagents-cli
      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: deepagents-${{ matrix.os }}
          path: target/release/deepagents
```

### 7.2 在 CI 中使用 deepagents

```yaml
- name: Generate changelog
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
    DEEPAGENTS_CODE_MODEL: openai:gpt-4o
  run: |
    git log --oneline HEAD~10..HEAD | \
    /path/to/deepagents --print > changelog.txt
```

---

## 八、版本路线图

### v0.1.0（当前）

- ✅ 单 prompt 模式 (`-p`)
- ✅ print 模式 (`--print`)
- ✅ OpenAI / Anthropic / Ollama provider
- ✅ `.env` 自动加载
- ✅ `--model` / `--name` / `--debug` 参数
- ⏳ 10 个子命令占位

### v0.2.0（计划）

- ⏳ `deepagents doctor` —— 环境诊断
- ⏳ `deepagents config --list` —— 配置查看
- ⏳ `deepagents update --check` —— 版本检查

### v0.3.0+（规划）

- ⏳ TUI 交互模式
- ⏳ `deepagents serve` —— HTTP API
- ⏳ 会话持久化（`deepagents resume`）
- ⏳ MCP 服务器集成
- ⏳ 插件 / 技能 / Hooks 系统
- ⏳ 沙箱执行
- ⏳ 成本追踪
- ⏳ Goal/Issue 自动循环

---

## 九、开发贡献指南

### 9.1 代码规范

- `#![forbid(unsafe_code)]` —— 禁止 unsafe
- `#![warn(missing_docs)]` —— 公开 item 必须有文档
- 所有 crate：`edition = 2024`，`rust-version = 1.88`
- 错误类型用 `thiserror`，统一 `deepagents_errors::Error`

### 9.2 测试基线

```bash
# 提交前必须全部通过
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# 预期：280 tests, 0 failures, 0 warnings
```

### 9.3 新增 Provider 检查清单

- [ ] 在 `ProviderModel` enum 加 variant
- [ ] 在 `ProviderModel::new()` 加 match arm
- [ ] 在 `CompletionModel` impl 的 `completion()` 加 arm
- [ ] 在 `CompletionModel` impl 的 `stream()` 加 arm
- [ ] 在 `default_model_for()` 加默认模型
- [ ] 在 `infer_provider()` 加环境变量检测
- [ ] 更新 `ProviderError::UnsupportedProvider` 错误消息
- [ ] 加单元测试
- [ ] 更新文档

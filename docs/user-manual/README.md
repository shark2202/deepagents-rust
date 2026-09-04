# deepagents 使用手册

> **LangChain Deep Agents SDK — Rust 实现，构建在 rig 之上**
>
> 版本：v0.1.0 ｜ Rust Edition 2024 ｜ MSRV 1.88

本手册覆盖从初级到高级、从用户到运维的全场景使用指南，共 7 篇文档。

---

## 📖 文档索引

### 用户手册

| 文档 | 适用对象 | 内容概要 |
|------|---------|---------|
| [初级用户手册](./01-user-beginner.md) | 第一次使用 deepagents 的新手 | 安装、配置第一个 LLM、跑通第一个 prompt |
| [中级用户手册](./02-user-intermediate.md) | 已跑通基础功能，想灵活配置 | 多 provider 配置、`.env` 进阶、`--model` / `--print` / `--name` / `--debug` |
| [高级用户手册](./03-user-advanced.md) | 需要精确控制解析逻辑、做扩展开发 | 6 级优先级链、内部调用链路、13 个入口点、编程式调用、新增 Provider |

### 运维手册

| 文档 | 适用对象 | 内容概要 |
|------|---------|---------|
| [初级运维手册](./04-ops-beginner.md) | 负责编译、安装、分发的实施人员 | 系统要求、编译、测试、安装方式（4 种）、交叉编译、Docker |
| [中级运维手册](./05-ops-intermediate.md) | 多环境部署、配置管理、排障 | 配置层次、多环境策略、Docker 部署、systemd、安全实践、常见排障 6 例 |
| [高级运维手册](./06-ops-advanced.md) | 资深运维/架构师 | 20 crate 架构图、关键技术决策、高级排障、性能调优、CI/CD、版本路线图 |

---

## ⚡ 快速开始

```bash
# 1. 克隆 & 编译
git clone https://github.com/shark2202/deepagents-rust.git
cd deepagents-rust
cargo build --release -p deepagents-cli

# 2. 配置 API Key（任选一种 provider）
echo 'OPENAI_API_KEY=sk-your-key' > .env

# 3. 跑通
./target/release/deepagents -p "你好"
```

→ 不清楚？从 [初级用户手册](./01-user-beginner.md) 开始。

---

## 🏗 当前功能状态

| 功能 | 状态 | 命令 |
|------|------|------|
| 单 prompt 模式 | ✅ 已实现 | `deepagents -p "你的问题"` |
| print 管道模式 | ✅ 已实现 | `echo "..." \| deepagents --print` |
| OpenAI provider | ✅ 已实现 | `OPENAI_API_KEY=...` |
| Anthropic provider | ✅ 已实现 | `ANTHROPIC_API_KEY=...` |
| Ollama provider | ✅ 已实现 | `OLLAMA_API_BASE_URL=...` |
| OpenAI 兼容 API（bbgate 等） | ✅ 已实现 | `OPENAI_BASE_URL=...` |
| `.env` 自动加载 | ✅ 已实现 | 当前目录 `.env` 文件 |
| `--model` 覆盖 | ✅ 已实现 | `--model "provider:model"` |
| TUI 交互模式 | ⏳ 占位 | `deepagents`（无参数） |
| `serve` HTTP API | ⏳ 占位 | `deepagents serve --port 8080` |
| `doctor` 诊断 | ⏳ 占位 | `deepagents doctor` |
| `config` 配置管理 | ⏳ 占位 | `deepagents config --list` |
| `update` 自更新 | ⏳ 占位 | `deepagents update --check` |
| `resume` 会话恢复 | ⏳ 占位 | `deepagents resume --session-id xxx` |
| `mcp` / `plugins` / `skills` / `hooks` | ⏳ 占位 | 见 [高级用户手册](./03-user-advanced.md) |

---

## 🔑 环境变量速查

| 变量 | 说明 |
|------|------|
| `OPENAI_API_KEY` | OpenAI / 兼容 API 密钥 |
| `OPENAI_BASE_URL` | 自定义 API 地址（第三方平台必设） |
| `ANTHROPIC_API_KEY` | Anthropic Claude 密钥 |
| `OLLAMA_API_BASE_URL` | Ollama 服务地址 |
| `DEEPAGENTS_CODE_MODEL` | `provider:model` 格式，直接指定 |
| `DEEPAGENTS_CODE_PROVIDER` | 只指定 provider，模型用默认 |

> 优先级：`--model` CLI 参数 > Shell 环境变量 > `.env` 文件 > 内置默认值

详见 [高级用户手册 · 优先级链](./03-user-advanced.md#一provider-解析-6-级优先级链)。

---

## 📂 文档目录结构

```
docs/user-manual/
├── README.md                  ← 你在这里（总索引）
├── 01-user-beginner.md        ← 初级用户手册
├── 02-user-intermediate.md    ← 中级用户手册
├── 03-user-advanced.md        ← 高级用户手册
├── 04-ops-beginner.md         ← 初级运维手册
├── 05-ops-intermediate.md     ← 中级运维手册
└── 06-ops-advanced.md         ← 高级运维手册
```

---

## 🤝 贡献

- 代码规范：`#![forbid(unsafe_code)]` + `#![warn(missing_docs)]`
- 提交前检查：`cargo fmt --check && cargo clippy -- -D warnings && cargo test --workspace`
- 测试基线：280 tests, 0 failures, 0 warnings
- 新增 Provider 指南见 [高级运维手册 §9.3](./06-ops-advanced.md)

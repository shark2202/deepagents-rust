# deepagents 运维手册 · 中级

> **适用对象**：需要在多环境（开发/测试/生产）部署 deepagents、管理配置、做基本排障的运维/实施工程师。
>
> **前置条件**：已读完 [运维初级手册](./04-ops-beginner.md)，能成功编译和安装 deepagents。

---

## 一、部署架构概览

deepagents 是一个**单二进制 CLI 工具**，无守护进程、无数据库、无持久化状态。它的运行依赖：

```
┌──────────────┐       ┌──────────────────┐       ┌─────────────────┐
│  deepagents  │──────▶│  LLM Provider    │       │  .env / 环境变量 │
│  (单二进制)   │       │  (OpenAI/Claude/ │       │  (配置来源)      │
│              │       │   Ollama/bbgate) │       │                 │
└──────────────┘       └──────────────────┘       └─────────────────┘
       │
       │  stdin/stdout
       ▼
┌──────────────┐
│  调用方       │
│  (Shell/CI)  │
└──────────────┘
```

### 部署模式

| 模式 | 场景 | 说明 |
|------|------|------|
| **本地交互** | 开发者个人使用 | `.env` 在项目目录，直接 `deepagents -p "..."` |
| **CI/CD 集成** | 自动化流水线 | 环境变量由 CI 注入，二进制放 PATH |
| **Docker 容器** | 标准化分发 | 二进制 + `.env` 挂载 |
| **服务端部署** | serve 子命令（未来） | 目前占位，未来启动 HTTP API |

---

## 二、配置管理

### 2.1 配置层次

deepagents 的配置来源从高到低：

```
优先级高 ────────────────────────────────────── 优先级低

  ┌─────────────────┐
  │ CLI 参数          │  --model, --name, --debug
  │ (最高优先级)      │  覆盖一切
  └────────┬────────┘
           │
  ┌────────▼────────┐
  │ Shell 环境变量    │  export KEY=val
  │                  │  覆盖 .env
  └────────┬────────┘
           │
  ┌────────▼────────┐
  │ .env 文件        │  当前工作目录
  │                  │  仅填补 Shell 未设的变量
  └────────┬────────┘
           │
  ┌────────▼────────┐
  │ 内置默认值        │  provider 默认模型等
  │ (最低优先级)      │  gpt-4o / claude-sonnet-4-5 / llama3.2
  └─────────────────┘
```

### 2.2 多环境配置策略

#### 策略 A：多 `.env` 文件 + 目录隔离

每个环境一个目录，各含自己的 `.env`：

```
/opt/deepagents/
├── bin/
│   └── deepagents              # 二进制
├── envs/
│   ├── dev/
│   │   └── .env                # OPENAI_API_KEY=sk-dev-key
│   ├── staging/
│   │   └── .env                # OPENAI_API_KEY=sk-staging-key
│   └── prod/
│       └── .env                # OPENAI_API_KEY=sk-prod-key
```

使用时切换工作目录：
```bash
cd /opt/deepagents/envs/prod && /opt/deepagents/bin/deepagents -p "hello"
```

#### 策略 B：Shell 环境变量注入

CI/CD 流水线中，用环境变量注入（不依赖 `.env` 文件）：

```yaml
# GitHub Actions 示例
- name: Run deepagents
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
    DEEPAGENTS_CODE_MODEL: openai:gpt-4o
  run: deepagents -p "Review this PR"
```

```bash
# Jenkins pipeline 示例
sh '''
  OPENAI_API_KEY=${PROD_OPENAI_KEY} \
  DEEPAGENTS_CODE_MODEL=openai:gpt-4o \
  /opt/deepagents/bin/deepagents -p "Generate release notes"
'''
```

#### 策略 C：Docker + 环境变量挂载

```dockerfile
FROM debian:bookworm-slim
COPY deepagents /usr/local/bin/
ENTRYPOINT ["deepagents"]
```

```bash
# 开发环境
docker run --rm \
  -e OPENAI_API_KEY=sk-dev-key \
  deepagents:0.1.0 -p "hello"

# 生产环境（挂载 .env 文件）
docker run --rm \
  --env-file /opt/deepagents/prod.env \
  deepagents:0.1.0 -p "hello"
```

### 2.3 配置变量清单

| 变量 | 必要性 | 说明 |
|------|--------|------|
| `OPENAI_API_KEY` | OpenAI 必设 | API 密钥 |
| `OPENAI_BASE_URL` | 第三方 API 必设 | 自定义 base URL（如 bbgate） |
| `ANTHROPIC_API_KEY` | Anthropic 必设 | API 密钥 |
| `OLLAMA_API_BASE_URL` | Ollama 必设 | Ollama 服务地址 |
| `OLLAMA_API_KEY` | Ollama 可选 | 认证密钥 |
| `DEEPAGENTS_CODE_MODEL` | 可选 | `provider:model` 格式，直接指定 |
| `DEEPAGENTS_CODE_PROVIDER` | 可选 | 只指定 provider |

---

## 三、日志与可观测性

### 3.1 日志输出

deepagents 的日志通过 `tracing` crate 输出到 **stderr**，正常输出到 **stdout**：

```bash
# 正常输出（stdout）→ 可以管道传递
deepagents -p "hello" | tee response.txt

# 错误/调试输出（stderr）→ 单独重定向
deepagents --debug -p "hello" 2> debug.log

# 分离 stdout 和 stderr
deepagents --debug -p "hello" > response.txt 2> debug.log
```

### 3.2 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 成功（含子命令占位） |
| `1` | 运行时错误（provider 解析失败、HTTP 错误等） |

```bash
deepagents -p "hello"
echo "exit code: $?"
# 0 → 成功
# 1 → 失败
```

### 3.3 调试模式

```bash
# 开启 debug
deepagents --debug -p "hello" 2>&1

# 完全静默（只看回复）
deepagents -p "hello" 2>/dev/null
```

---

## 四、健康检查

### 4.1 二进制完整性

```bash
deepagents --version
# deepagents 0.1.0

deepagents --help > /dev/null 2>&1
echo "exit: $?"
# 0 → 二进制正常
```

### 4.2 Provider 连通性

```bash
# OpenAI 连通性
deepagents -p "ping" --model "openai:gpt-4o"
# 如果返回 LLM 回复 → OpenAI 通
# 如果报 HttpError → 检查 API key / 网络 / base URL

# Ollama 连通性
deepagents -p "ping" --model "ollama:llama3.2"
# 如果报连接失败 → Ollama 服务没启动

# bbgate 连通性
OPENAI_API_KEY=sk-xxx OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1 \
  deepagents -p "ping" --model "openai:z-ai/glm-5.2"
```

### 4.3 无 API Key 时的优雅降级

```bash
# 应该输出友好错误，不应 panic
env -u OPENAI_API_KEY -u ANTHROPIC_API_KEY -u OLLAMA_API_BASE_URL \
  deepagents -p "hello"
# deepagents: no LLM provider configured: ...
echo "exit: $?"
# 1
```

---

## 五、安全最佳实践

### 5.1 API Key 保护

- ✅ `.env` 已在 `.gitignore` 中（`.env` 和 `.env.*`）
- ✅ 用 Docker secret 或 CI secret 注入，不在镜像/代码中硬编码
- ✅ 定期轮换 API Key
- ❌ 永远不要在日志/Slack/issue 中粘贴 API Key

### 5.2 Docker 安全

```bash
# 以非 root 用户运行
docker run --rm \
  --user 1000:1000 \
  --env-file /path/to/secrets.env \
  --read-only \
  --tmpfs /tmp \
  deepagents:0.1.0 -p "hello"
```

### 5.3 网络安全

deepagents 只需要**出站 HTTPS** 到 LLM provider 的 API 端点。不需要任何入站端口（当前版本）。

```
出站规则：
  → api.openai.com:443       (OpenAI)
  → api.anthropic.com:443   (Anthropic)
  → localhost:11434          (Ollama 本地)
  → token.bytebroad.com.cn:443 (bbgate)
```

---

## 六、Docker 部署详解

### 6.1 多阶段构建

```dockerfile
# ── Stage 1: Build ──
FROM rust:1.88-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p deepagents-cli

# ── Stage 2: Runtime ──
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/deepagents /usr/local/bin/
ENTRYPOINT ["deepagents"]
CMD ["--help"]
```

```bash
docker build -t deepagents:0.1.0 .
# 镜像大小约 150-200 MB
```

### 6.2 精简镜像（musl 静态链接）

```dockerfile
FROM rust:1.88-slim AS builder
WORKDIR /app
COPY . .
RUN rustup target add x86_64-unknown-linux-musl && \
    cargo build --release -p deepagents-cli --target x86_64-unknown-linux-musl

FROM alpine:latest
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/deepagents /usr/local/bin/
ENTRYPOINT ["deepagents"]
```

```bash
docker build -t deepagents:0.1.0-alpine .
# 镜像大小约 50 MB
```

---

## 七、systemd 服务部署（未来 serve 模式）

当前 `serve` 子命令尚未实现，以下是未来部署参考：

```ini
# /etc/systemd/system/deepagents.service
[Unit]
Description=DeepAgents HTTP API Server
After=network.target

[Service]
Type=simple
User=deepagents
WorkingDirectory=/opt/deepagents
EnvironmentFile=/opt/deepagents/.env
ExecStart=/opt/deepagents/bin/deepagents serve --port 8080
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable deepagents
sudo systemctl start deepagents
sudo journalctl -u deepagents -f
```

> ⚠️ 当前版本不支持 `serve`，上述配置仅作未来规划参考。

---

## 八、常见排障

### 问题 1：编译报错 "edition 2024 is unstable"

```
error: edition 2024 is unstable
```

**原因**：Rust 版本低于 1.85。

**解决**：
```bash
rustup update stable
rustc --version  # 应 >= 1.88.0
```

### 问题 2：编译报错 "rust-version 1.88"

```
error: package 'deepagents' requires rustc 1.88 or newer
```

**解决**：同上，升级 Rust 工具链。

### 问题 3：运行报 "no LLM provider configured"

```
deepagents: no LLM provider configured: set OPENAI_API_KEY, ...
```

**排查步骤**：
```bash
# 1. 检查 .env 文件是否存在且内容正确
cat .env

# 2. 检查环境变量是否生效
env | grep -E "OPENAI|ANTHROPIC|OLLAMA|DEEPAGENTS"

# 3. 显式设置测试
OPENAI_API_KEY=sk-xxx deepagents -p "hello"

# 4. 检查当前工作目录
pwd  # .env 必须在当前目录下
```

### 问题 4：运行报 HttpError 连接 OpenAI 官方

```
deepagents: agent run failed: CompletionError: HttpError: error sending request for url (https://api.openai.com/v1/chat/completions)
```

**原因**：设了 `OPENAI_API_KEY` 但没设 `OPENAI_BASE_URL`，请求发到了 OpenAI 官方。在第三方 API（bbgate 等）场景下会失败。

**解决**：
```bash
# 检查 .env 是否有 OPENAI_BASE_URL
grep OPENAI_BASE_URL .env
# 应输出：OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1

# 如果没有，加上它
echo 'OPENAI_BASE_URL=https://token.bytebroad.com.cn/v1' >> .env
```

### 问题 5：Ollama 连接失败

```
deepagents: agent run failed: CompletionError: HttpError: error sending request for url (http://localhost:11434/api/chat)
```

**原因**：Ollama 服务未运行或端口不对。

**解决**：
```bash
# 检查 Ollama 是否运行
curl http://localhost:11434/api/tags

# 如果连接拒绝，启动 Ollama
ollama serve &

# 确认模型已拉取
ollama list
# NAME       ID     SIZE    MODIFIED
# llama3.2   ...    ...     ...

# 如果没有，拉取
ollama pull llama3.2
```

### 问题 6：Docker 中无法访问宿主机 Ollama

```
error sending request for url (http://localhost:11434/api/chat)
```

**原因**：Docker 容器内 `localhost` 指向容器自身，不是宿主机。

**解决**：
```bash
# macOS / Linux Docker Desktop
docker run --rm \
  -e OLLAMA_API_BASE_URL=http://host.docker.internal:11434 \
  deepagents:0.1.0 -p "hello"

# Linux（无 Docker Desktop）
docker run --rm \
  -e OLLAMA_API_BASE_URL=http://172.17.0.1:11434 \
  --network host \
  deepagents:0.1.0 -p "hello"
```

---

## 下一步

- 要深入架构、crate 依赖图、高级排障？→ [运维手册（高级）](./06-ops-advanced.md)

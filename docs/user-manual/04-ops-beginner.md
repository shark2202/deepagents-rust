# deepagents 运维手册 · 初级

> **适用对象**：负责编译、安装、分发给用户的实施/运维人员。
>
> **前置条件**：了解 Rust 基本概念（crate、workspace、cargo），有一台 build 机器。

---

## 一、系统要求

### 1.1 Rust 工具链

| 项目 | 最低版本 | 推荐 |
|------|---------|------|
| `rustc` | **1.88.0** | latest stable |
| `cargo` | **1.88.0** | latest stable |
| edition | **2024** | 2024 |
| 组件 | `rustfmt`、`clippy` | 可选但推荐 |

检查：
```bash
rustc --version
# rustc 1.88.0 (or newer)

cargo --version
# cargo 1.88.0 (or newer)
```

如果版本不够，升级：
```bash
rustup update stable
```

> **注意**：项目使用 `edition = "2024"`，需要 Rust 1.85+ 才能编译 edition 2024 的代码，但 `rust-version = "1.88"` 是 MSRV（Minimum Supported Rust Version），低于 1.88 会直接报错。

### 1.2 操作系统

| OS | 状态 | 备注 |
|----|------|------|
| macOS (x86_64 / ARM64) | ✅ 已验证 | 主要开发平台 |
| Linux (x86_64) | ✅ 支持 | CI 标准 |
| Linux (aarch64) | ✅ 支持 | 如树莓派 5 |
| Windows | ⚠️ 未测试 | 理论支持，可能有路径/API 兼容问题 |

### 1.3 系统依赖

项目使用 `rusqlite` 的 `bundled` feature（自带 SQLite 源码编译），**不需要系统安装 SQLite**。其余依赖（OpenSSL 等）通过 `rustls-tls` 纯 Rust TLS 解决。

**macOS**：无需额外系统依赖。
```bash
# 只需要 Xcode Command Line Tools
xcode-select --install
```

**Linux (Debian/Ubuntu)**：
```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config curl
```

**Linux (CentOS/RHEL)**：
```bash
sudo yum groupinstall -y "Development Tools"
```

### 1.4 磁盘与内存

| 资源 | 最低 | 推荐 |
|------|------|------|
| 磁盘（源码 + 编译缓存） | 2 GB | 5 GB |
| 内存（编译时） | 2 GB | 4 GB+ |
| 编译时间（debug） | 3-5 分钟 | — |
| 编译时间（release） | 8-15 分钟 | — |

> 首次编译会下载并编译所有依赖（rig-core、reqwest、tokio 等），耗时较长。后续增量编译在 10 秒内。

---

## 二、获取源码

```bash
git clone https://github.com/shark2202/deepagents-rust.git
cd deepagents-rust
```

确认 workspace 完整性：
```bash
# 应列出 20 个 crate
ls crates/ | wc -l
# 20

# 检查 workspace members
cargo metadata --no-deps --format-version 1 2>/dev/null | \
  python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d['packages']))"
# 21 (20 crates + workspace root)
```

---

## 三、编译

### 3.1 Debug 编译（日常开发）

```bash
cargo build -p deepagents-cli
```

产物位置：`target/debug/deepagents`

### 3.2 Release 编译（分发用）

```bash
cargo build --release -p deepagents-cli
```

产物位置：`target/release/deepagents`

> Release 编译启用优化（`opt-level = 3`），二进制更小、运行更快，但编译时间更长。

### 3.3 编译全部 crate

```bash
# Debug
cargo build --workspace

# Release
cargo build --release --workspace
```

### 3.4 只编译二进制（不含 lib 和测试）

```bash
# 最快的方式
cargo build -p deepagents-cli --bin deepagents
```

---

## 四、测试

### 4.1 全量测试

```bash
cargo test --workspace
```

当前基线：**280 个测试，0 失败，0 警告**。

### 4.2 只测单个 crate

```bash
cargo test -p deepagents-runtime
cargo test -p deepagents-cli
cargo test -p deepagents-env
```

### 4.3 只跑 doc test

```bash
cargo test --workspace --doc
```

### 4.4 Clippy 检查

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

### 4.5 格式检查

```bash
cargo fmt --all -- --check
```

---

## 五、安装二进制

### 方式一：`cargo install`（系统级安装）

```bash
cargo install --path crates/deepagents-cli
```

安装到 `~/.cargo/bin/deepagents`（需在 PATH 中）。验证：
```bash
deepagents --version
# deepagents 0.1.0
```

### 方式二：手动拷贝

```bash
# 编译 release
cargo build --release -p deepagents-cli

# 拷贝到系统路径
sudo cp target/release/deepagents /usr/local/bin/

# 或用户级安装
mkdir -p ~/.local/bin
cp target/release/deepagents ~/.local/bin/
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### 方式三：构建分发包

```bash
# 编译 release
cargo build --release -p deepagents-cli

# 创建分发包
mkdir -p dist/deepagents-0.1.0
cp target/release/deepagents dist/deepagents-0.1.0/
cp -r docs/ dist/deepagents-0.1.0/
cat > dist/deepagents-0.1.0/README.md << 'EOF'
# deepagents 0.1.0

## 安装
sudo cp deepagents /usr/local/bin/

## 使用
deepagents -p "hello"
EOF

# 打包
cd dist
tar czf deepagents-0.1.0-darwin-x86_64.tar.gz deepagents-0.1.0
```

### 方式四：Docker

```dockerfile
FROM rust:1.88-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p deepagents-cli

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/deepagents /usr/local/bin/
ENTRYPOINT ["deepagents"]
```

```bash
docker build -t deepagents:0.1.0 .
docker run --rm -e OPENAI_API_KEY=sk-xxx deepagents:0.1.0 -p "hello"
```

---

## 六、交叉编译

### macOS ARM64 → Linux x86_64

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --release -p deepagents-cli --target x86_64-unknown-linux-gnu
```

> 可能需要安装交叉链接器：`brew install filosottile/musl-cross/musl-cross` 并用 `x86_64-unknown-linux-musl` target。

### macOS → Linux musl（静态链接）

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release -p deepagents-cli --target x86_64-unknown-linux-musl
```

产物是完全静态链接的二进制，可在任何 Linux 上运行。

---

## 七、验证安装成功

```bash
# 1. 版本
deepagents --version
# deepagents 0.1.0

# 2. 帮助
deepagents --help

# 3. 无 API key 时的优雅报错
env -u OPENAI_API_KEY -u ANTHROPIC_API_KEY -u OLLAMA_API_BASE_URL \
  deepagents -p "hello"
# deepagents: no LLM provider configured: ...

# 4. 有 API key 时真实调用
echo 'OPENAI_API_KEY=sk-your-key' > .env
deepagents -p "hello"
# （LLM 回复）
```

---

## 下一步

- 要做多环境部署、配置管理？→ [运维手册（中级）](./05-ops-intermediate.md)
- 遇到架构问题或需要性能调优？→ [运维手册（高级）](./06-ops-advanced.md)

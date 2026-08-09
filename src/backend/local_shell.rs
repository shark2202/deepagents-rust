//! `LocalShellBackend` —— 对应 deepagents `LocalShellBackend`（subprocess execute on host）。
//!
//! Phase 2a 增量 A：只实现 `execute`（沙箱），文件操作继承 default not-implemented。
//! deepagents 原版继承 `FilesystemBackend`（host fs 文件操作）；本 crate 的 `FilesystemBackend`
//! 是 root-scoped 虚拟路径，语义不同，故 `LocalShellBackend` 暂不混入——
//! caller 需要本地文件+shell 时用 `CompositeBackend`（增量 C）路由。MVP 先只 execute。

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::process::Command;

use super::protocol::{Backend, SandboxBackend};
use super::types::*;

/// 本地 shell backend：直接在 host 跑 shell 命令。
#[derive(Debug, Clone)]
pub struct LocalShellBackend {
    cwd: Option<PathBuf>,
    id: String,
}

impl LocalShellBackend {
    /// 在当前工作目录执行。
    #[must_use]
    pub fn new() -> Self {
        Self {
            cwd: None,
            id: "local-shell".to_string(),
        }
    }

    /// 指定工作目录。
    #[must_use]
    pub fn with_cwd(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: Some(cwd.into()),
            id: "local-shell".to_string(),
        }
    }
}

impl Default for LocalShellBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for LocalShellBackend {
    fn supported_tools(&self) -> Vec<&'static str> {
        // 文件操作继承 default not-implemented；仅 execute 可用。
        vec!["execute"]
    }

    fn as_sandbox(&self) -> Option<&dyn SandboxBackend> {
        Some(self)
    }
}

#[async_trait]
impl SandboxBackend for LocalShellBackend {
    fn id(&self) -> &str {
        &self.id
    }

    async fn execute(&self, command: &str, timeout: Option<u32>) -> ExecuteResponse {
        // 跨平台 shell：unix sh -c, windows cmd /C。
        let (program, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        let mut cmd = Command::new(program);
        cmd.arg(flag).arg(command);
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        let spawn = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let child = match spawn {
            Ok(c) => c,
            Err(e) => return ExecuteResponse::new(e.to_string(), None),
        };
        let fut = child.wait_with_output();
        let out = match timeout {
            Some(secs) => {
                match tokio::time::timeout(std::time::Duration::from_secs(u64::from(secs)), fut)
                    .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        return ExecuteResponse::new(
                            format!("Error: execute timed out after {secs}s"),
                            None,
                        );
                    }
                }
            }
            None => fut.await,
        };
        match out {
            Ok(o) => {
                let mut output = String::new();
                output.push_str(&String::from_utf8_lossy(&o.stdout));
                output.push_str(&String::from_utf8_lossy(&o.stderr));
                ExecuteResponse::new(output, o.status.code())
            }
            Err(e) => ExecuteResponse::new(e.to_string(), None),
        }
    }
}

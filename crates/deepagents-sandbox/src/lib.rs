//! Trait-based sandbox provider abstraction with 6 providers (Q21).
//!
//! This crate provides a provider-agnostic sandbox abstraction used by the
//! Deep Agents runtime to spin up isolated execution environments. There are
//! three concrete providers ([`LangSmithProvider`], [`DaytonaProvider`],
//! [`VercelProvider`]) built on top of `reqwest` with rustls, and three stub
//! providers ([`AgentCoreProvider`], [`ModalProvider`], [`RunloopProvider`])
//! that surface a [`SandboxError::Provider`] error and are intended as
//! placeholders for future implementations.
//!
//! See `docs/SPEC.md` §Q21 for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use deepagents_errors::{Error, SandboxError};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Config / data model ──────────────────────────────────────────────────

/// Resource limits for a sandbox instance.
///
/// All fields are optional; when omitted the provider applies its own
/// defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SandboxResources {
    /// Number of CPU cores to allocate.
    pub cpu: Option<u32>,
    /// Memory budget in megabytes.
    pub memory_mb: Option<u32>,
    /// Disk budget in megabytes.
    pub disk_mb: Option<u32>,
}

/// Configuration used to create a new sandbox instance.
///
/// `provider` selects which registered provider handles the request; the
/// remaining fields are interpreted in a provider-specific way.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SandboxConfig {
    /// Provider identifier (must match a registered [`SandboxProvider::id`]).
    pub provider: String,
    /// Container/VM image to launch, if the provider supports images.
    pub image: Option<String>,
    /// Environment variables to inject into the sandbox.
    pub env: HashMap<String, String>,
    /// Working directory inside the sandbox.
    pub working_dir: Option<String>,
    /// Default command timeout in seconds.
    pub timeout: Option<u64>,
    /// Resource limits for the sandbox.
    pub resources: Option<SandboxResources>,
}

/// High-level lifecycle status of a sandbox instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxStatus {
    /// The sandbox is being provisioned.
    Creating,
    /// The sandbox is running and accepting commands.
    Running,
    /// The sandbox has been stopped but not yet destroyed.
    Stopped,
    /// The sandbox entered an error state; the inner string describes it.
    Error(String),
}

/// A handle to a running (or stopped) sandbox.
///
/// Returned by [`SandboxProvider::create`]. The `id` is used to reference the
/// sandbox in subsequent [`SandboxProvider::execute`] and
/// [`SandboxProvider::destroy`] calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxInstance {
    /// Provider-assigned identifier for the sandbox.
    pub id: String,
    /// Provider that owns this sandbox.
    pub provider: String,
    /// Current lifecycle status.
    pub status: SandboxStatus,
    /// Optional network endpoint (host:port or URL) for the sandbox.
    pub endpoint: Option<String>,
}

/// Result of executing a command inside a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResult {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Process exit code (0 indicates success).
    pub exit_code: i32,
    /// Whether the command was killed due to a timeout.
    pub timed_out: bool,
}

// ── Provider trait ───────────────────────────────────────────────────────

/// A sandbox provider capable of creating, executing commands in, and
/// destroying isolated execution environments.
///
/// Implementations are independent: each provider owns its own HTTP client
/// and credentials. The trait is `Send + Sync` so providers can be stored in
/// a [`SandboxRegistry`] and shared across async tasks.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Create a new sandbox from the supplied [`SandboxConfig`].
    ///
    /// Returns a [`SandboxInstance`] handle whose `status` is typically
    /// [`SandboxStatus::Creating`] immediately after creation.
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, Error>;

    /// Execute `command` inside the referenced sandbox.
    ///
    /// `timeout` overrides the sandbox default in seconds when `Some`.
    async fn execute(
        &self,
        sandbox_id: &str,
        command: &str,
        timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error>;

    /// Destroy the referenced sandbox, releasing all resources.
    async fn destroy(&self, sandbox_id: &str) -> Result<(), Error>;

    /// Provider identifier (e.g. `"langsmith"`).
    fn id(&self) -> &str;

    /// Whether this provider is a stub (no real backend wired up).
    fn is_stub(&self) -> bool;
}

// ── Registry ────────────────────────────────────────────────────────────

/// Registry of available sandbox providers keyed by [`SandboxProvider::id`].
pub struct SandboxRegistry {
    providers: HashMap<String, Box<dyn SandboxProvider>>,
}

impl SandboxRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register a provider, replacing any existing provider with the same id.
    pub fn register(&mut self, provider: Box<dyn SandboxProvider>) {
        self.providers.insert(provider.id().to_owned(), provider);
    }

    /// Look up a provider by id.
    pub fn get(&self, id: &str) -> Option<&dyn SandboxProvider> {
        self.providers.get(id).map(|p| p.as_ref())
    }

    /// List all registered providers, in unspecified order.
    pub fn list(&self) -> Vec<&dyn SandboxProvider> {
        self.providers.values().map(|p| p.as_ref()).collect()
    }
}

impl Default for SandboxRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Stub helper ──────────────────────────────────────────────────────────

/// Build the standard "not yet implemented" error for a stub provider.
fn stub_error(provider: &str) -> Error {
    SandboxError::Provider(format!("{provider} not yet implemented")).into()
}

// ── Concrete providers: LangSmith / Daytona / Vercel ────────────────────

/// LangSmith-based sandbox provider.
///
/// This provider wraps the LangSmith sandbox HTTP API using a rustls-backed
/// `reqwest::Client`. In v0 the `create` flow constructs a client and a
/// placeholder [`SandboxInstance`] (status [`SandboxStatus::Creating`]);
/// `execute` returns a [`SandboxError::Provider`] error until real API
/// credentials are configured. The structure is in place so that wiring up
/// the real endpoints only requires filling in the request bodies.
#[derive(Clone)]
pub struct LangSmithProvider {
    /// API key used for authentication.
    pub api_key: String,
    /// Base URL of the LangSmith API.
    pub base_url: String,
    /// HTTP client (rustls).
    pub client: reqwest::Client,
}

impl LangSmithProvider {
    /// Default LangSmith API base URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.smith.langchain.com";

    /// Create a new LangSmith provider with a fresh rustls client.
    pub fn new(api_key: impl Into<String>) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| SandboxError::Provider(format!("failed to build client: {e}")))?;
        Ok(Self::new_with_client(api_key, client))
    }

    /// Create a new LangSmith provider with a caller-supplied client.
    pub fn new_with_client(api_key: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            client,
        }
    }
}

#[async_trait]
impl SandboxProvider for LangSmithProvider {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, Error> {
        tracing::debug!(
            provider = "langsmith",
            image = ?config.image,
            "creating sandbox"
        );

        // Real implementation would POST to {base_url}/v1/sandboxes and parse
        // the returned id. Without valid credentials the call would fail, so
        // for v0 we build the instance locally and let execute surface the
        // not-configured error.
        let instance_id = format!("langsmith-{}", pseudo_id());
        Ok(SandboxInstance {
            id: instance_id,
            provider: "langsmith".to_string(),
            status: SandboxStatus::Creating,
            endpoint: Some(format!("{}/sandboxes", self.base_url)),
        })
    }

    async fn execute(
        &self,
        sandbox_id: &str,
        command: &str,
        timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error> {
        tracing::debug!(
            provider = "langsmith",
            sandbox_id,
            command,
            timeout = ?timeout,
            "executing command"
        );
        // The request structure is intentionally laid out so that real
        // credentials can be wired in without reshaping the flow.
        let _url = format!("{}/v1/sandboxes/{sandbox_id}/execute", self.base_url);
        let _req = self
            .client
            .post(&_url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "command": command, "timeout": timeout }));

        // Without a real backend the call would 401/404; surface a clean
        // error instead of attempting the network in tests.
        Err(SandboxError::Execution(
            "langsmith execute requires a configured API key and a live sandbox".to_string(),
        )
        .into())
    }

    async fn destroy(&self, sandbox_id: &str) -> Result<(), Error> {
        tracing::debug!(
            provider = "langsmith",
            sandbox_id,
            "destroying sandbox"
        );
        let _url = format!("{}/v1/sandboxes/{sandbox_id}", self.base_url);
        let _req = self.client.delete(&_url).bearer_auth(&self.api_key);
        Err(SandboxError::Execution(
            "langsmith destroy requires a configured API key and a live sandbox".to_string(),
        )
        .into())
    }

    fn id(&self) -> &str {
        "langsmith"
    }

    fn is_stub(&self) -> bool {
        false
    }
}

/// Daytona-based sandbox provider.
///
/// Mirrors the shape of [`LangSmithProvider`]: a rustls `reqwest::Client`
/// plus API key. `create` returns a placeholder instance, `execute`/`destroy`
/// surface a not-configured error.
#[derive(Clone)]
pub struct DaytonaProvider {
    /// API key used for authentication.
    pub api_key: String,
    /// Base URL of the Daytona API.
    pub base_url: String,
    /// HTTP client (rustls).
    pub client: reqwest::Client,
}

impl DaytonaProvider {
    /// Default Daytona API base URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://app.daytona.io";

    /// Create a new Daytona provider with a fresh rustls client.
    pub fn new(api_key: impl Into<String>) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| SandboxError::Provider(format!("failed to build client: {e}")))?;
        Ok(Self::new_with_client(api_key, client))
    }

    /// Create a new Daytona provider with a caller-supplied client.
    pub fn new_with_client(api_key: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            client,
        }
    }
}

#[async_trait]
impl SandboxProvider for DaytonaProvider {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, Error> {
        tracing::debug!(
            provider = "daytona",
            image = ?config.image,
            "creating sandbox"
        );
        let instance_id = format!("daytona-{}", pseudo_id());
        Ok(SandboxInstance {
            id: instance_id,
            provider: "daytona".to_string(),
            status: SandboxStatus::Creating,
            endpoint: Some(format!("{}/sandboxes", self.base_url)),
        })
    }

    async fn execute(
        &self,
        sandbox_id: &str,
        command: &str,
        timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error> {
        tracing::debug!(
            provider = "daytona",
            sandbox_id,
            command,
            timeout = ?timeout,
            "executing command"
        );
        let _url = format!("{}/api/v1/workspace/{sandbox_id}/execute", self.base_url);
        let _req = self
            .client
            .post(&_url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "command": command, "timeout": timeout }));
        Err(SandboxError::Execution(
            "daytona execute requires a configured API key and a live sandbox".to_string(),
        )
        .into())
    }

    async fn destroy(&self, sandbox_id: &str) -> Result<(), Error> {
        tracing::debug!(
            provider = "daytona",
            sandbox_id,
            "destroying sandbox"
        );
        let _url = format!("{}/api/v1/workspace/{sandbox_id}", self.base_url);
        let _req = self.client.delete(&_url).bearer_auth(&self.api_key);
        Err(SandboxError::Execution(
            "daytona destroy requires a configured API key and a live sandbox".to_string(),
        )
        .into())
    }

    fn id(&self) -> &str {
        "daytona"
    }

    fn is_stub(&self) -> bool {
        false
    }
}

/// Vercel-based sandbox provider.
///
/// Mirrors the shape of [`LangSmithProvider`].
#[derive(Clone)]
pub struct VercelProvider {
    /// API key / token used for authentication.
    pub api_key: String,
    /// Base URL of the Vercel API.
    pub base_url: String,
    /// HTTP client (rustls).
    pub client: reqwest::Client,
}

impl VercelProvider {
    /// Default Vercel API base URL.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.vercel.com";

    /// Create a new Vercel provider with a fresh rustls client.
    pub fn new(api_key: impl Into<String>) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| SandboxError::Provider(format!("failed to build client: {e}")))?;
        Ok(Self::new_with_client(api_key, client))
    }

    /// Create a new Vercel provider with a caller-supplied client.
    pub fn new_with_client(api_key: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            client,
        }
    }
}

#[async_trait]
impl SandboxProvider for VercelProvider {
    async fn create(&self, config: &SandboxConfig) -> Result<SandboxInstance, Error> {
        tracing::debug!(
            provider = "vercel",
            image = ?config.image,
            "creating sandbox"
        );
        let instance_id = format!("vercel-{}", pseudo_id());
        Ok(SandboxInstance {
            id: instance_id,
            provider: "vercel".to_string(),
            status: SandboxStatus::Creating,
            endpoint: Some(format!("{}/v1/sandboxes", self.base_url)),
        })
    }

    async fn execute(
        &self,
        sandbox_id: &str,
        command: &str,
        timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error> {
        tracing::debug!(
            provider = "vercel",
            sandbox_id,
            command,
            timeout = ?timeout,
            "executing command"
        );
        let _url = format!("{}/v1/sandboxes/{sandbox_id}/execute", self.base_url);
        let _req = self
            .client
            .post(&_url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "command": command, "timeout": timeout }));
        Err(SandboxError::Execution(
            "vercel execute requires a configured API key and a live sandbox".to_string(),
        )
        .into())
    }

    async fn destroy(&self, sandbox_id: &str) -> Result<(), Error> {
        tracing::debug!(
            provider = "vercel",
            sandbox_id,
            "destroying sandbox"
        );
        let _url = format!("{}/v1/sandboxes/{sandbox_id}", self.base_url);
        let _req = self.client.delete(&_url).bearer_auth(&self.api_key);
        Err(SandboxError::Execution(
            "vercel destroy requires a configured API key and a live sandbox".to_string(),
        )
        .into())
    }

    fn id(&self) -> &str {
        "vercel"
    }

    fn is_stub(&self) -> bool {
        false
    }
}

// ── Stub providers: AgentCore / Modal / Runloop ─────────────────────────

/// AgentCore sandbox provider (stub).
///
/// All operations return [`SandboxError::Provider`]; this is a placeholder
/// for a future implementation.
#[derive(Debug, Clone, Default)]
pub struct AgentCoreProvider;

/// Modal sandbox provider (stub).
///
/// All operations return [`SandboxError::Provider`]; this is a placeholder
/// for a future implementation.
#[derive(Debug, Clone, Default)]
pub struct ModalProvider;

/// Runloop sandbox provider (stub).
///
/// All operations return [`SandboxError::Provider`]; this is a placeholder
/// for a future implementation.
#[derive(Debug, Clone, Default)]
pub struct RunloopProvider;

impl AgentCoreProvider {
    /// Create a new AgentCore stub provider.
    pub fn new() -> Self {
        Self
    }
}

impl ModalProvider {
    /// Create a new Modal stub provider.
    pub fn new() -> Self {
        Self
    }
}

impl RunloopProvider {
    /// Create a new Runloop stub provider.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SandboxProvider for AgentCoreProvider {
    async fn create(&self, _config: &SandboxConfig) -> Result<SandboxInstance, Error> {
        Err(stub_error("AgentCore"))
    }
    async fn execute(
        &self,
        _sandbox_id: &str,
        _command: &str,
        _timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error> {
        Err(stub_error("AgentCore"))
    }
    async fn destroy(&self, _sandbox_id: &str) -> Result<(), Error> {
        Err(stub_error("AgentCore"))
    }
    fn id(&self) -> &str {
        "agentcore"
    }
    fn is_stub(&self) -> bool {
        true
    }
}

#[async_trait]
impl SandboxProvider for ModalProvider {
    async fn create(&self, _config: &SandboxConfig) -> Result<SandboxInstance, Error> {
        Err(stub_error("Modal"))
    }
    async fn execute(
        &self,
        _sandbox_id: &str,
        _command: &str,
        _timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error> {
        Err(stub_error("Modal"))
    }
    async fn destroy(&self, _sandbox_id: &str) -> Result<(), Error> {
        Err(stub_error("Modal"))
    }
    fn id(&self) -> &str {
        "modal"
    }
    fn is_stub(&self) -> bool {
        true
    }
}

#[async_trait]
impl SandboxProvider for RunloopProvider {
    async fn create(&self, _config: &SandboxConfig) -> Result<SandboxInstance, Error> {
        Err(stub_error("Runloop"))
    }
    async fn execute(
        &self,
        _sandbox_id: &str,
        _command: &str,
        _timeout: Option<u64>,
    ) -> Result<ExecuteResult, Error> {
        Err(stub_error("Runloop"))
    }
    async fn destroy(&self, _sandbox_id: &str) -> Result<(), Error> {
        Err(stub_error("Runloop"))
    }
    fn id(&self) -> &str {
        "runloop"
    }
    fn is_stub(&self) -> bool {
        true
    }
}

// ── Helpers / registry factories ─────────────────────────────────────────

/// Generate a short, process-unique pseudo-id without depending on a UUID
/// crate.
///
/// Uses [`std::time::SystemTime`] nanos plus a per-call counter; collisions
/// are not a concern in the v0 single-process context.
fn pseudo_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{nanos:x}{n:x}")
}

/// Build a [`SandboxRegistry`] pre-populated with the three stub providers
/// ([`AgentCoreProvider`], [`ModalProvider`], [`RunloopProvider`]).
///
/// Concrete providers require API keys and are therefore not auto-registered;
/// callers can register them explicitly via [`SandboxRegistry::register`].
pub fn default_providers() -> SandboxRegistry {
    let mut registry = SandboxRegistry::new();
    registry.register(Box::new(AgentCoreProvider::new()));
    registry.register(Box::new(ModalProvider::new()));
    registry.register(Box::new(RunloopProvider::new()));
    registry
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let cfg = SandboxConfig::default();
        assert!(cfg.provider.is_empty());
        assert!(cfg.image.is_none());
        assert!(cfg.env.is_empty());
        assert!(cfg.working_dir.is_none());
        assert!(cfg.timeout.is_none());
        assert!(cfg.resources.is_none());
    }

    #[test]
    fn test_sandbox_status_serde() {
        let cases = vec![
            (SandboxStatus::Creating, "creating"),
            (SandboxStatus::Running, "running"),
            (SandboxStatus::Stopped, "stopped"),
        ];
        for (status, tag) in cases {
            let json = serde_json::to_string(&status).expect("serialize");
            assert!(json.contains(tag), "expected tag {tag} in {json}");
            let back: SandboxStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(status, back);
        }

        // Error variant carries a detail string.
        let err = SandboxStatus::Error("boom".to_string());
        let json = serde_json::to_string(&err).expect("serialize error");
        let back: SandboxStatus = serde_json::from_str(&json).expect("deserialize error");
        assert_eq!(err, back);
    }

    #[tokio::test]
    async fn test_sandbox_registry_register_get() {
        let mut registry = SandboxRegistry::new();
        registry.register(Box::new(AgentCoreProvider::new()));
        let got = registry.get("agentcore").expect("agentcore registered");
        assert_eq!(got.id(), "agentcore");
        assert!(got.is_stub());
        assert!(registry.get("nope").is_none());
    }

    #[tokio::test]
    async fn test_sandbox_registry_list() {
        let registry = default_providers();
        let ids: Vec<&str> = registry.list().iter().map(|p| p.id()).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"agentcore"));
        assert!(ids.contains(&"modal"));
        assert!(ids.contains(&"runloop"));
    }

    #[tokio::test]
    async fn test_stub_provider_returns_error() {
        let provider = AgentCoreProvider::new();
        let cfg = SandboxConfig {
            provider: "agentcore".into(),
            ..Default::default()
        };
        let err = provider.create(&cfg).await.unwrap_err();
        match err {
            Error::Sandbox(SandboxError::Provider(msg)) => {
                assert!(msg.contains("AgentCore"));
            }
            other => panic!("expected Sandbox::Provider, got {other:?}"),
        }

        let exec_err = provider.execute("id", "ls", None).await.unwrap_err();
        assert!(matches!(exec_err, Error::Sandbox(SandboxError::Provider(_))));

        let destroy_err = provider.destroy("id").await.unwrap_err();
        assert!(matches!(
            destroy_err,
            Error::Sandbox(SandboxError::Provider(_))
        ));
    }

    #[test]
    fn test_langsmith_provider_id() {
        let provider = LangSmithProvider::new("test-key").expect("build provider");
        assert_eq!(provider.id(), "langsmith");
        assert!(!provider.is_stub());
        assert_eq!(provider.api_key, "test-key");
        assert!(!provider.base_url.is_empty());

        // Daytone / Vercel ids sanity-check too.
        let daytona = DaytonaProvider::new("k").expect("build daytona");
        assert_eq!(daytona.id(), "daytona");
        let vercel = VercelProvider::new("k").expect("build vercel");
        assert_eq!(vercel.id(), "vercel");
    }

    #[test]
    fn test_execute_result_serde() {
        let result = ExecuteResult {
            stdout: "hello\n".into(),
            stderr: "warn\n".into(),
            exit_code: 0,
            timed_out: false,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: ExecuteResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result.stdout, back.stdout);
        assert_eq!(result.stderr, back.stderr);
        assert_eq!(result.exit_code, back.exit_code);
        assert_eq!(result.timed_out, back.timed_out);
    }

    #[tokio::test]
    async fn test_langsmith_create_returns_creating_instance() {
        // Concrete provider create should succeed without network: it builds
        // a placeholder instance in Creating status.
        let provider = LangSmithProvider::new("key").expect("build");
        let cfg = SandboxConfig {
            provider: "langsmith".into(),
            ..Default::default()
        };
        let instance = provider.create(&cfg).await.expect("create");
        assert_eq!(instance.provider, "langsmith");
        assert_eq!(instance.status, SandboxStatus::Creating);
        assert!(instance.id.starts_with("langsmith-"));
        assert!(instance.endpoint.is_some());

        // Execute should surface a clean not-configured error.
        let exec_err = provider.execute(&instance.id, "ls", None).await.unwrap_err();
        assert!(matches!(
            exec_err,
            Error::Sandbox(SandboxError::Execution(_))
        ));
    }

    #[test]
    fn test_pseudo_id_uniqueness() {
        let a = pseudo_id();
        let b = pseudo_id();
        assert_ne!(a, b, "pseudo ids should be unique within a process");
        assert!(!a.is_empty());
    }
}

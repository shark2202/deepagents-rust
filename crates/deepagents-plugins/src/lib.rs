//! JSON-RPC stdio, gix, rust-embed adapter (Q16)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.
//!
//! This crate implements the plugin/extension subsystem described in spec
//! §Q16. Plugins and third-party sandbox providers communicate with the host
//! over a JSON-RPC 2.0 connection carried on stdio, mirroring the transport
//! used by LSP and MCP. A small Python adapter script is embedded into the
//! binary via `rust-embed` and extracted to a temp directory at runtime; git
//! operations (marketplace clones, manifest fetches) are performed with the
//! pure-Rust `gix` crate so the crate has zero C dependencies.
//!
//! # Migration note
//! Python native extensions built via PyO3 are *not* portable across a
//! pure-Rust build. The adapter bundled here is therefore a thin,
//! dependency-free bridge rather than a compiled extension; long-term the
//! project keeps everything on the Rust side of the FFI boundary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use deepagents_errors::{Error, PluginError};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// File name of a plugin manifest on disk.
pub const MANIFEST_FILE: &str = "plugin_manifest.json";

/// File name of the extension trust list on disk.
pub const TRUST_FILE: &str = "extension_trust.json";

/// JSON-RPC protocol version implemented by this transport.
pub const JSONRPC_VERSION: &str = "2.0";

/// Embedded assets folder.
///
/// At present this embeds the Python JSON-RPC adapter bridge
/// (`assets/adapter.py`) which the host extracts to a temp directory and
/// spawns via `python3`. New assets can be dropped into `assets/` without
/// touching the rest of the crate.
#[derive(Embed)]
#[folder = "assets/"]
struct PluginAsset;

// ── Manifest / source / state ─────────────────────────────────────────────

/// A plugin manifest, normally loaded from `plugin_manifest.json`.
///
/// Mirrors the manifest schema declared in spec §Q16: a plugin declares its
/// name, version, entry point, requested permissions, and the tools it
/// exposes. `description` is optional and shown in listings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin name (also the lookup key in the trust list).
    pub name: String,
    /// Semver-ish version string supplied by the author.
    pub version: String,
    /// Entry point invoked to spawn the plugin process. For a Python adapter
    /// this is `python3 <extracted adapter path>`; for a native binary it is
    /// the path to the executable.
    pub entry_point: String,
    /// Permissions the plugin requests (e.g. `filesystem`, `network`).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Tools the plugin advertises over JSON-RPC.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Optional human-readable description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Where a plugin was installed from.
///
/// Serialized as `snake_case` so on-disk representations stay stable and
/// human-readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    /// Installed from a local path on disk.
    Local,
    /// Cloned from a git remote.
    Git,
    /// Fetched from a marketplace registry.
    Marketplace,
}

/// Runtime state of an installed plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    /// Plugin is enabled and may be spawned.
    Enabled,
    /// Plugin is disabled and will not be spawned.
    Disabled,
    /// Plugin is in an error state; the string describes the failure.
    Error(String),
}

/// An installed plugin, combining its manifest, origin, runtime state, and
/// on-disk install path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    /// The plugin manifest.
    pub manifest: PluginManifest,
    /// Where the plugin was installed from.
    pub source: PluginSource,
    /// Current runtime state.
    pub state: PluginState,
    /// Absolute (or install-root-relative) path to the plugin directory.
    pub path: String,
}

// ── Trust list ────────────────────────────────────────────────────────────

/// A single trust-list entry recording whether a plugin is trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    /// Name of the plugin this entry refers to.
    pub plugin_name: String,
    /// Whether the user has manually trusted this plugin.
    pub trusted: bool,
    /// Unix timestamp (seconds) when the entry was added/updated.
    pub added_at: i64,
}

/// The extension trust list, persisted as `extension_trust.json`.
///
/// Unknown plugins are treated as untrusted by default; callers should
/// always route through [`TrustList::is_trusted`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustList {
    /// Map of plugin name -> trust entry.
    pub entries: HashMap<String, TrustEntry>,
}

impl TrustList {
    /// Create an empty trust list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a plugin as trusted, recording the supplied timestamp.
    pub fn trust(&mut self, name: &str, added_at: i64) {
        self.entries.insert(
            name.to_string(),
            TrustEntry {
                plugin_name: name.to_string(),
                trusted: true,
                added_at,
            },
        );
    }

    /// Mark a plugin as untrusted, recording the supplied timestamp.
    pub fn untrust(&mut self, name: &str, added_at: i64) {
        self.entries.insert(
            name.to_string(),
            TrustEntry {
                plugin_name: name.to_string(),
                trusted: false,
                added_at,
            },
        );
    }

    /// Returns whether the named plugin is trusted. Unknown plugins report
    /// `false`.
    pub fn is_trusted(&self, name: &str) -> bool {
        self.entries
            .get(name)
            .map(|e| e.trusted)
            .unwrap_or(false)
    }

    /// Parse a trust list from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        Ok(serde_json::from_str(json)?)
    }

    /// Serialize the trust list to a JSON string.
    pub fn to_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

// ── JSON-RPC types ─────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version, normally [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Request id. `None` signals a notification (no response expected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    /// Method name being invoked.
    pub method: String,
    /// Optional parameters payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Build a request with the standard protocol version.
    pub fn new(method: &str, id: serde_json::Value, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id),
            method: method.to_string(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code (negative for protocol-defined errors).
    pub code: i32,
    /// Short human-readable error message.
    pub message: String,
    /// Optional structured data about the error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 response object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Protocol version, normally [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// id of the request this response corresponds to.
    pub id: serde_json::Value,
    /// Result payload (present on success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error payload (present on failure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

// ── Plugin transport (JSON-RPC over stdio) ─────────────────────────────────

/// A JSON-RPC 2.0 connection carried over a child process's stdio.
///
/// The host spawns a plugin (or a third-party sandbox provider) as a child
/// process, writes one JSON-RPC request per line to the child's stdin, and
/// reads one JSON-RPC response per line from the child's stdout.
///
/// # Non-`Clone` note
/// This type intentionally does **not** implement `Clone`: it owns a
/// [`tokio::process::Child`] together with its `stdin`/`stdout` pipes, none
/// of which can be safely duplicated. There is exactly one owner of the
/// transport per running plugin process; if you need shared access, wrap it
/// in an `Arc<Mutex<PluginTransport>>`.
pub struct PluginTransport {
    /// The spawned child process.
    child: Child,
    /// Piped stdin of the child.
    stdin: ChildStdin,
    /// Buffered reader over the child's stdout.
    stdout: BufReader<ChildStdout>,
}

impl std::fmt::Debug for PluginTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginTransport")
            .field("child", &"tokio::process::Child")
            .finish()
    }
}

impl PluginTransport {
    /// Spawn a plugin process for the given entry point and own its stdio.
    ///
    /// The `entry_point` string is interpreted via the shell (`sh -c` on
    /// Unix, `cmd /C` on Windows) so manifest authors can write things like
    /// `python3 /tmp/adapter.py`.
    pub async fn spawn(entry_point: &str) -> Result<Self, Error> {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(entry_point);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(entry_point);
            c
        };
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Plugin(PluginError::Extension("no stdin pipe".to_string())))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Plugin(PluginError::Extension("no stdout pipe".to_string())))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// Send a JSON-RPC request and read back the single-line response.
    ///
    /// The request is serialized to one line of JSON and written to the
    /// child's stdin (followed by a newline and flush). The next non-empty
    /// line read from the child's stdout is parsed as a
    /// [`JsonRpcResponse`]. If the response carries an `error`, it is
    /// converted into [`PluginError::Extension`].
    pub async fn send_request(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, Error> {
        let line = serde_json::to_string(&request)?;
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(Error::Io)?;
        self.stdin.write_all(b"\n").await.map_err(Error::Io)?;
        self.stdin.flush().await.map_err(Error::Io)?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self
                .stdout
                .read_line(&mut buf)
                .await
                .map_err(Error::Io)?;
            if n == 0 {
                return Err(Error::Plugin(PluginError::Extension(
                    "plugin stdout closed before response".to_string(),
                )));
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resp: JsonRpcResponse = serde_json::from_str(trimmed)?;
            if let Some(err) = resp.error.clone() {
                return Err(Error::Plugin(PluginError::Extension(format!(
                    "{}: {}",
                    err.code, err.message
                ))));
            }
            return Ok(resp);
        }
    }

    /// Kill the child process and release its resources.
    pub async fn close(&mut self) -> Result<(), Error> {
        // Best-effort close of stdin so the child sees EOF.
        let _ = self.stdin.shutdown().await;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        Ok(())
    }
}

impl Drop for PluginTransport {
    fn drop(&mut self) {
        // Reap the child if it is still around; ignore failures.
        let _ = self.child.start_kill();
        let _ = self.child.try_wait();
    }
}

// ── Embedded Python adapter ───────────────────────────────────────────────

/// Extracts the embedded Python adapter (`adapter.py`) to a temp directory
/// and returns the path to it. This is the entry point the host spawns when
/// a plugin's manifest references the bundled adapter.
///
/// The destination is `<tmp>/deepagents-plugin-adapter-<pid>-<counter>/adapter.py`,
/// unique per process and per call so concurrent extractions do not collide.
pub fn extract_adapter() -> Result<PathBuf, Error> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("deepagents-plugin-adapter-{pid}-{id}"));
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join("adapter.py");
    if let Some(file) = PluginAsset::get("adapter.py") {
        std::fs::write(&dest, file.data.as_ref())?;
    } else {
        return Err(Error::Plugin(PluginError::Extension(
            "embedded adapter.py not found".to_string(),
        )));
    }
    Ok(dest)
}

// ── Plugin manager ─────────────────────────────────────────────────────────

/// Owns the installed plugin set, the trust list, and the install root.
pub struct PluginManager {
    /// Installed plugins, in insertion order.
    pub plugins: Vec<Plugin>,
    /// The trust list.
    pub trust_list: TrustList,
    /// Directory plugins are installed under.
    pub install_dir: PathBuf,
}

impl std::fmt::Debug for PluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginManager")
            .field("plugins", &self.plugins)
            .field("trust_list", &self.trust_list)
            .field("install_dir", &self.install_dir)
            .finish()
    }
}

impl PluginManager {
    /// Create a new manager rooted at `install_dir`.
    pub fn new<P: Into<PathBuf>>(install_dir: P) -> Self {
        Self {
            plugins: Vec::new(),
            trust_list: TrustList::new(),
            install_dir: install_dir.into(),
        }
    }

    /// Load and parse a `plugin_manifest.json` from `path` (a directory or a
    /// file path).
    pub fn load_manifest<P: AsRef<Path>>(&self, path: P) -> Result<PluginManifest, Error> {
        let p = path.as_ref();
        let file_path = if p.is_dir() {
            p.join(MANIFEST_FILE)
        } else {
            p.to_path_buf()
        };
        let data = std::fs::read_to_string(&file_path).map_err(|e| {
            Error::Plugin(PluginError::Manifest(format!(
                "failed to read {}: {e}",
                file_path.display()
            )))
        })?;
        let manifest: PluginManifest = serde_json::from_str(&data).map_err(|e| {
            Error::Plugin(PluginError::Manifest(format!(
                "failed to parse {}: {e}",
                file_path.display()
            )))
        })?;
        Ok(manifest)
    }

    /// Install a plugin from an already-loaded manifest.
    ///
    /// The plugin is recorded as [`PluginState::Disabled`] until explicitly
    /// enabled; callers should also update the trust list separately.
    pub fn install_from_manifest(
        &mut self,
        manifest: PluginManifest,
        source: PluginSource,
        path: impl Into<String>,
    ) -> Result<(), Error> {
        let plugin = Plugin {
            manifest,
            source,
            state: PluginState::Disabled,
            path: path.into(),
        };
        self.plugins.push(plugin);
        Ok(())
    }

    /// Enable a plugin by name. A missing plugin yields
    /// [`PluginError::State`].
    pub fn enable(&mut self, name: &str) -> Result<(), Error> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| {
                Error::Plugin(PluginError::State(format!("plugin not found: {name}")))
            })?;
        plugin.state = PluginState::Enabled;
        Ok(())
    }

    /// Disable a plugin by name. A missing plugin yields
    /// [`PluginError::State`].
    pub fn disable(&mut self, name: &str) -> Result<(), Error> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|p| p.manifest.name == name)
            .ok_or_else(|| {
                Error::Plugin(PluginError::State(format!("plugin not found: {name}")))
            })?;
        plugin.state = PluginState::Disabled;
        Ok(())
    }

    /// Borrow the installed plugins.
    pub fn list(&self) -> &[Plugin] {
        &self.plugins
    }

    /// Returns whether the named plugin is trusted (delegating to the trust
    /// list; unknown plugins are untrusted).
    pub fn is_trusted(&self, name: &str) -> bool {
        self.trust_list.is_trusted(name)
    }
}

// ── Marketplace client (v0 stub) ───────────────────────────────────────────

/// v0 stub for git-based marketplace operations.
///
/// A full marketplace client would clone a remote repository, verify a
/// checksum, read the manifest, and offer update/uninstall flows. Those
/// pieces are large and depend on registry conventions not yet finalized,
/// so the v0 surface here is intentionally minimal and returns
/// "not yet implemented" errors so callers can detect and degrade
/// gracefully.
#[derive(Debug, Clone, Default)]
pub struct MarketplaceClient {
    // reserved for future config (registry base URL, auth, cache dir, ...)
}

impl MarketplaceClient {
    /// Create a new marketplace client.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clone a marketplace repository into `dest`. v0 stub.
    ///
    /// A real implementation will use `gix` to perform a shallow clone;
    /// until the registry conventions are finalized this returns a
    /// [`PluginError::Marketplace`] "not yet implemented" error.
    pub async fn clone_repo(&self, _url: &str, _dest: &Path) -> Result<(), Error> {
        Err(Error::Plugin(PluginError::Marketplace(
            "clone_repo not yet implemented".to_string(),
        )))
    }

    /// Fetch a plugin manifest from a marketplace repository. v0 stub.
    pub async fn fetch_manifest(&self, _repo_url: &str) -> Result<PluginManifest, Error> {
        Err(Error::Plugin(PluginError::Marketplace(
            "fetch_manifest not yet implemented".to_string(),
        )))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Atomic counter for unique temp subdir names within the test process.
    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_tmp(sub: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("deepagents-plugins-test-{pid}-{n}-{sub}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn now_ts() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    #[test]
    fn test_plugin_manifest_serde() {
        let manifest = PluginManifest {
            name: "echo".to_string(),
            version: "0.1.0".to_string(),
            entry_point: "python3 adapter.py".to_string(),
            permissions: vec!["filesystem".to_string()],
            tools: vec!["echo".to_string()],
            description: Some("an echo plugin".to_string()),
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "echo");
        assert_eq!(back.version, "0.1.0");
        assert_eq!(back.entry_point, "python3 adapter.py");
        assert_eq!(back.permissions, vec!["filesystem".to_string()]);
        assert_eq!(back.tools, vec!["echo".to_string()]);
        assert_eq!(back.description.as_deref(), Some("an echo plugin"));

        // Defaults: missing permissions/tools deserialize to empty.
        let minimal = r#"{"name":"n","version":"1","entry_point":"e"}"#;
        let m: PluginManifest = serde_json::from_str(minimal).unwrap();
        assert!(m.permissions.is_empty());
        assert!(m.tools.is_empty());
        assert!(m.description.is_none());
    }

    #[test]
    fn test_plugin_source_state_serde() {
        // snake_case round-trips.
        let s = serde_json::to_string(&PluginSource::Marketplace).unwrap();
        assert_eq!(s, "\"marketplace\"");
        let s: PluginSource = serde_json::from_str("\"git\"").unwrap();
        assert_eq!(s, PluginSource::Git);

        let st = serde_json::to_string(&PluginState::Enabled).unwrap();
        assert_eq!(st, "\"enabled\"");
        let st = serde_json::to_string(&PluginState::Error("boom".into())).unwrap();
        assert!(st.contains("\"error\""));
        assert!(st.contains("\"boom\""));
        let parsed: PluginState = serde_json::from_str(&st).unwrap();
        assert_eq!(parsed, PluginState::Error("boom".to_string()));
    }

    #[test]
    fn test_trust_list() {
        let mut tl = TrustList::new();
        assert!(!tl.is_trusted("alpha"));
        tl.trust("alpha", 10);
        assert!(tl.is_trusted("alpha"));
        tl.untrust("alpha", 20);
        assert!(!tl.is_trusted("alpha"));
        // Unknown stays untrusted.
        assert!(!tl.is_trusted("beta"));

        let json = tl.to_json().unwrap();
        let back = TrustList::from_json(&json).unwrap();
        assert!(!back.is_trusted("alpha"));
        assert_eq!(back.entries.len(), tl.entries.len());

        // Re-trust and verify round-trip preserves trusted=true.
        tl.trust("gamma", 30);
        let json = tl.to_json().unwrap();
        let back = TrustList::from_json(&json).unwrap();
        assert!(back.is_trusted("gamma"));
    }

    #[test]
    fn test_jsonrpc_request_serde() {
        let req = JsonRpcRequest::new(
            "initialize",
            serde_json::json!(42),
            Some(serde_json::json!({"a": 1})),
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":42"));
        assert!(json.contains("\"method\":\"initialize\""));
        let back: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "initialize");
        assert_eq!(back.id, Some(serde_json::json!(42)));

        // Notification (no id) is omitted from JSON.
        let notif = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: None,
            method: "shutdown".to_string(),
            params: None,
        };
        let j = serde_json::to_string(&notif).unwrap();
        assert!(!j.contains("\"id\""));
        assert!(!j.contains("\"params\""));
    }

    #[test]
    fn test_jsonrpc_response_serde() {
        let ok = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: serde_json::json!(7),
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        let j = serde_json::to_string(&ok).unwrap();
        assert!(j.contains("\"result\""));
        assert!(!j.contains("\"error\""));
        let back: JsonRpcResponse = serde_json::from_str(&j).unwrap();
        assert_eq!(back.id, serde_json::json!(7));

        let err = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: serde_json::json!(7),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "method not found".to_string(),
                data: Some(serde_json::json!({"method": "foo"})),
            }),
        };
        let j = serde_json::to_string(&err).unwrap();
        assert!(j.contains("\"error\""));
        assert!(!j.contains("\"result\""));
        let back: JsonRpcResponse = serde_json::from_str(&j).unwrap();
        let e = back.error.expect("error present");
        assert_eq!(e.code, -32601);
        assert_eq!(e.message, "method not found");
    }

    #[test]
    fn test_plugin_manager_load_manifest() {
        let dir = unique_tmp("load");
        let manifest = PluginManifest {
            name: "loader".to_string(),
            version: "0.2.0".to_string(),
            entry_point: "python3 /tmp/adapter.py".to_string(),
            permissions: vec!["network".to_string()],
            tools: vec!["fetch".to_string()],
            description: None,
        };
        let path = dir.join(MANIFEST_FILE);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes())
            .unwrap();
        drop(f);

        let mgr = PluginManager::new(dir.clone());
        // Load via directory path.
        let m = mgr.load_manifest(&dir).unwrap();
        assert_eq!(m.name, "loader");
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.tools, vec!["fetch".to_string()]);
        // Load via explicit file path.
        let m = mgr.load_manifest(&path).unwrap();
        assert_eq!(m.name, "loader");

        // Missing file -> Manifest error variant.
        let err = mgr.load_manifest(dir.join("nope.json")).unwrap_err();
        match err {
            Error::Plugin(PluginError::Manifest(_)) => {}
            other => panic!("expected Manifest error, got {other:?}"),
        }
    }

    #[test]
    fn test_plugin_manager_enable_disable() {
        let dir = unique_tmp("enable");
        let mut mgr = PluginManager::new(&dir);
        let manifest = PluginManifest {
            name: "flip".to_string(),
            version: "0.1.0".to_string(),
            entry_point: "python3 x.py".to_string(),
            permissions: vec![],
            tools: vec![],
            description: None,
        };
        mgr.install_from_manifest(manifest, PluginSource::Local, dir.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].state, PluginState::Disabled);

        // enable
        mgr.enable("flip").unwrap();
        assert_eq!(mgr.list()[0].state, PluginState::Enabled);
        // disable
        mgr.disable("flip").unwrap();
        assert_eq!(mgr.list()[0].state, PluginState::Disabled);

        // Unknown plugin -> State error.
        let err = mgr.enable("nope").unwrap_err();
        match err {
            Error::Plugin(PluginError::State(_)) => {}
            other => panic!("expected State error, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_adapter() {
        let path = extract_adapter().unwrap();
        assert!(path.ends_with("adapter.py"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("JSONRPC_VERSION"));
        assert!(content.contains("def main"));
    }

    #[tokio::test]
    async fn test_plugin_transport_roundtrip() {
        // Use `python3` if available to echo a JSON-RPC response; otherwise skip.
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        // Adapter handles one request -> one response line. Spawn it directly.
        let adapter_path = extract_adapter().unwrap();
        let entry = format!("python3 {}", adapter_path.to_string_lossy());
        let mut tx = PluginTransport::spawn(&entry).await.unwrap();

        let req = JsonRpcRequest::new(
            "initialize",
            serde_json::json!(1),
            Some(serde_json::json!({})),
        );
        let resp = tx.send_request(req).await.unwrap();
        assert_eq!(resp.id, serde_json::json!(1));
        let result = resp.result.expect("result present");
        assert!(result.get("adapter").is_some());
        tx.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_marketplace_stubs() {
        let client = MarketplaceClient::new();
        let err = client.clone_repo("x", Path::new("/tmp/x")).await.unwrap_err();
        match err {
            Error::Plugin(PluginError::Marketplace(_)) => {}
            other => panic!("expected Marketplace error, got {other:?}"),
        }
        let err = client.fetch_manifest("x").await.unwrap_err();
        match err {
            Error::Plugin(PluginError::Marketplace(_)) => {}
            other => panic!("expected Marketplace error, got {other:?}"),
        }
        let _ = now_ts(); // keep helper referenced even if unused
    }
}

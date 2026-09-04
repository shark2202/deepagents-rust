//! rmcp, .mcp.json, trust lists, OAuth (Q19)
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` §19 for the full design specification.
//!
//! This crate implements the configuration layer for Model Context Protocol
//! (MCP) clients: parsing `.mcp.json` server configs, evaluating trust
//! (reject-wins allow/reject lists), managing OAuth token caches, and
//! expanding `${VAR}` / `${VAR:-default}` env vars in config values.
//!
//! The actual MCP transport (stdio/sse/http) is intentionally left as a v0
//! stub: the official Rust MCP SDK (`rmcp`) is not in the current dependency
//! set, so [`McpClient::connect`] returns [`McpError::Transport`] until rmcp
//! is wired in. The data model and serialization shapes are designed so rmcp
//! can be slotted in later without breaking the public API.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use deepagents_errors::{Error, McpError};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── 1. Transport ────────────────────────────────────────────────────────

/// The transport mechanism an MCP server speaks.
///
/// Serialized as snake_case (e.g. `"stdio"`, `"sse"`, `"http"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    /// Spawn a local process and talk over its stdin/stdout.
    Stdio,
    /// Server-Sent Events stream (legacy; rmcp may not support this transport).
    Sse,
    /// Streamable HTTP transport.
    Http,
}

// ── 3. Trust level ──────────────────────────────────────────────────────

/// Trust classification for an MCP server.
///
/// Determines whether the agent may auto-connect to the server or must first
/// prompt the user for approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Explicitly trusted by the user; may auto-connect.
    Trusted,
    /// Explicitly untrusted; connection is refused.
    Untrusted,
    /// Trust decision deferred — user must approve before connecting.
    Pending,
}

impl Default for TrustLevel {
    /// New servers default to [`TrustLevel::Pending`]: never auto-trust.
    fn default() -> Self {
        Self::Pending
    }
}

// ── 2. Server config ────────────────────────────────────────────────────

/// Configuration for a single MCP server entry from `.mcp.json`.
///
/// A server is either a local stdio process (in which case `command`/`args`
/// are set) or a remote network server (in which case `url` is set). The
/// `transport` field disambiguates the wire protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Human-readable server name; used as the lookup key in [`McpConfig`].
    pub name: String,
    /// Wire transport this server speaks.
    pub transport: McpTransport,
    /// For `stdio`: the executable to spawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// For `stdio`: arguments to pass to the command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// For `sse`/`http`: the server URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Extra environment variables to set for the spawned process.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Trust classification for this server.
    #[serde(default)]
    pub trust: TrustLevel,
}

// ── 4. Top-level config ─────────────────────────────────────────────────

/// Top-level `.mcp.json` configuration: the set of known servers plus the
/// global allow/reject lists used to gate connections.
///
/// Trust list evaluation is **reject-wins**: a server name in `reject_list`
/// is always denied, and if `allow_list` is non-empty only names in it are
/// permitted (everything else denied). When `allow_list` is empty, any name
/// not explicitly rejected is allowed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// Configured MCP servers.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
    /// Global reject list. Any match here denies a server (reject-wins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reject_list: Vec<String>,
    /// Global allow list. When non-empty, only these names are permitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_list: Vec<String>,
}

impl McpConfig {
    /// Create an empty config (no servers, no allow/reject lists).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a server to the config.
    pub fn add_server(&mut self, server: McpServerConfig) {
        self.servers.push(server);
    }

    /// Parse a `.mcp.json` document into a [`McpConfig`].
    ///
    /// Returns [`McpError::Config`] on a parse failure.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        let config: Self = serde_json::from_str(json)
            .map_err(|e| McpError::Config(format!("failed to parse .mcp.json: {e}")))?;
        Ok(config)
    }

    /// Serialize this config back to a `.mcp.json` document string.
    pub fn to_json(&self) -> Result<String, Error> {
        let s = serde_json::to_string_pretty(self)?;
        Ok(s)
    }

    /// Decide whether a server name is permitted to connect.
    ///
    /// Reject-wins semantics:
    /// - If `name` is in `reject_list`, return `false`.
    /// - If `allow_list` is non-empty and `name` is not in it, return `false`.
    /// - Otherwise return `true`.
    #[must_use]
    pub fn is_server_allowed(&self, name: &str) -> bool {
        if self.reject_list.iter().any(|n| n == name) {
            return false;
        }
        if !self.allow_list.is_empty() && !self.allow_list.iter().any(|n| n == name) {
            return false;
        }
        true
    }

    /// Look up a server config by name.
    #[must_use]
    pub fn resolve_server(&self, name: &str) -> Option<&McpServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }
}

// ── 5/6. OAuth ─────────────────────────────────────────────────────────

/// The OAuth flow an MCP server uses for authentication.
///
/// Serialized as snake_case (e.g. `"loopback_redirect"`, `"device_code"`,
/// `"paste_back"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthFlow {
    /// Authorization-code flow with a loopback redirect URI.
    LoopbackRedirect,
    /// Device authorization grant (user visits a URL and enters a code).
    DeviceCode,
    /// Manual paste-back: user copies a token out-of-band and pastes it in.
    PasteBack,
}

/// OAuth client configuration for an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// Which of the three supported OAuth flows to use.
    pub flow: OAuthFlow,
    /// OAuth client id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// OAuth client secret (only for confidential clients).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Requested OAuth scopes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Redirect URI (used by the loopback-redirect flow).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
}

impl Default for OAuthConfig {
    /// Default to the manual [`OAuthFlow::PasteBack`] flow (no client id/secret
    /// or network listener required).
    fn default() -> Self {
        Self {
            flow: OAuthFlow::PasteBack,
            client_id: None,
            client_secret: None,
            scopes: Vec::new(),
            redirect_uri: None,
        }
    }
}

// ── 7/8. Token cache ───────────────────────────────────────────────────

/// A cached OAuth token for an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenEntry {
    /// The server this token belongs to.
    pub server_name: String,
    /// OAuth access token.
    pub access_token: String,
    /// Optional refresh token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the access token expires, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// On-disk cache of MCP OAuth tokens, one JSON file per server.
///
/// Files live under `{cache_dir}/{server_name}.json`. The default cache
/// directory is `~/.deepagents/.state/mcp-tokens/`.
#[derive(Debug, Clone)]
pub struct TokenCache {
    /// Directory holding the per-server token JSON files.
    pub cache_dir: PathBuf,
}

impl TokenCache {
    /// Create a cache rooted at `cache_dir`.
    #[must_use]
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
        }
    }

    /// The default token cache directory: `~/.deepagents/.state/mcp-tokens/`.
    ///
    /// Returns a relative `mcp-tokens` path if the home directory cannot be
    /// resolved (callers should treat that as an error condition upstream).
    #[must_use]
    pub fn default_dir() -> PathBuf {
        let mut p = deepagents_home_dir().unwrap_or_else(|| PathBuf::from("."));
        p.push(".state");
        p.push("mcp-tokens");
        p
    }

    /// Persist a token entry to `{cache_dir}/{server_name}.json`.
    ///
    /// Creates the cache directory if it does not yet exist.
    pub fn store(&self, entry: TokenEntry) -> Result<(), Error> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let path = self.cache_dir.join(format!("{}.json", entry.server_name));
        let json = serde_json::to_string_pretty(&entry)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load a token entry for `server_name`, if present in the cache.
    ///
    /// Returns `Ok(None)` if no cache file exists for the server.
    pub fn load(&self, server_name: &str) -> Result<Option<TokenEntry>, Error> {
        let path = self.cache_dir.join(format!("{server_name}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let entry: TokenEntry = serde_json::from_str(&data)?;
        Ok(Some(entry))
    }

    /// Delete the cached token entry for `server_name`, if any.
    ///
    /// A missing file is not an error.
    pub fn remove(&self, server_name: &str) -> Result<(), Error> {
        let path = self.cache_dir.join(format!("{server_name}.json"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Check whether a token entry is expired.
    ///
    /// A token with no `expires_at` is never considered expired. A token
    /// whose `expires_at` is in the past is expired.
    #[must_use]
    pub fn is_expired(entry: &TokenEntry) -> bool {
        match entry.expires_at {
            Some(exp) => {
                let now = current_unix_seconds();
                now >= exp
            }
            None => false,
        }
    }
}

// ── 9. Env expansion ───────────────────────────────────────────────────

/// Expand `${VAR}` and `${VAR:-default}` env-var references in `input`.
///
/// Uses the actual process environment. Unknown variables with no default
/// expand to the empty string.
///
/// # Examples
///
/// ```
/// use deepagents_mcp::expand_env_with;
/// use std::collections::HashMap;
/// let mut env = HashMap::new();
/// env.insert("HOME".to_string(), "/root".to_string());
/// assert_eq!(expand_env_with("${HOME}/sub", &env), "/root/sub");
/// assert_eq!(expand_env_with("${MISSING:-fallback}", &env), "fallback");
/// ```
pub fn expand_env(input: &str) -> String {
    let env: HashMap<String, String> = std::env::vars().collect();
    expand_env_with(input, &env)
}

/// Expand `${VAR}` and `${VAR:-default}` using the provided env map instead of
/// the real process environment.
///
/// Unknown variables with no default expand to the empty string. Non-`${...}`
/// text is passed through verbatim.
pub fn expand_env_with(input: &str, env: &HashMap<String, String>) -> String {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    while i < len {
        if i + 1 < len && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            // Find closing '}'.
            let start = i + 2;
            let mut j = start;
            while j < len && bytes[j] != b'}' {
                j += 1;
            }
            if j >= len {
                // No closing brace; emit literally and stop.
                out.push_str(&input[i..]);
                break;
            }
            // body between start..j (exclusive of '}')
            let body = &input[start..j];
            // Advance past "${...}"
            i = j + 1;

            // Split on first ":-" for default value.
            let (name, default) = match body.find(":-") {
                Some(idx) => (&body[..idx], Some(&body[idx + 2..])),
                None => (body, None),
            };

            if let Some(val) = env.get(name) {
                if !val.is_empty() {
                    out.push_str(val);
                } else if let Some(d) = default {
                    out.push_str(d);
                }
            } else if let Some(d) = default {
                out.push_str(d);
            }
            // else: unknown + no default → empty
        } else {
            // Emit one char.
            let ch = input[i..].chars().next().expect("char exists");
            out.push(ch);
            i += ch.len_utf8();
        }
    }

    out
}

// ── 10/11/12. Client + connection + tool info ─────────────────────────

/// A placeholder handle to a live MCP connection.
///
/// In the v0 stub this carries only the resolved server name and transport.
/// Once `rmcp` is wired in, this will wrap the real rmcp client handle.
#[derive(Debug, Clone)]
pub struct McpConnection {
    /// Name of the server this connection is bound to.
    pub server_name: String,
    /// Transport the connection is using.
    pub transport: McpTransport,
}

/// Metadata about a tool exposed by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
}

/// v0 MCP client: holds config and a token cache but cannot yet open a real
/// transport (rmcp is not in the dependency set).
///
/// The data model here is shaped so that wiring in `rmcp` later only requires
/// implementing [`McpClient::connect`] / [`McpClient::list_tools`] bodies; the
/// public signatures stay stable.
#[derive(Debug, Clone)]
pub struct McpClient {
    /// The loaded MCP server config + trust lists.
    pub config: McpConfig,
    /// OAuth token cache used for authenticated servers.
    pub token_cache: TokenCache,
}

impl McpClient {
    /// Construct a new MCP client from a config and token cache.
    #[must_use]
    pub fn new(config: McpConfig, token_cache: TokenCache) -> Self {
        Self {
            config,
            token_cache,
        }
    }

    /// Connect to an MCP server by name.
    ///
    /// **v0 stub.** The actual MCP transport requires the `rmcp` crate, which
    /// is not yet a dependency. This always returns
    /// [`McpError::Transport`].
    pub async fn connect(&self, server_name: &str) -> Result<McpConnection, Error> {
        // Resolve + validate trust before attempting any transport work.
        let server = self.config.resolve_server(server_name).ok_or_else(|| {
            McpError::Resolution(format!("unknown MCP server: {server_name}"))
        })?;
        if !self.config.is_server_allowed(server_name) {
            return Err(McpError::Resolution(format!(
                "MCP server '{server_name}' is blocked by the trust lists"
            ))
            .into());
        }
        // v0: no real transport. rmcp wiring is a migration gap (see SPEC §19).
        // We carry `server` into the (future) rmcp call site by referencing it
        // here so the resolution above is observably used.
        Err(McpError::Transport(format!(
            "MCP transport not yet implemented for server '{name}' ({transport:?}); requires rmcp crate",
            name = server.name,
            transport = server.transport,
        ))
        .into())
    }

    /// List the tools exposed by an already-connected MCP server.
    ///
    /// **v0 stub.** Always returns [`McpError::Transport`] until rmcp is wired
    /// in and [`McpConnection`] carries a real client handle.
    pub async fn list_tools(
        &self,
        _conn: &McpConnection,
    ) -> Result<Vec<McpToolInfo>, Error> {
        Err(McpError::Transport(
            "MCP tool listing not yet implemented, requires rmcp crate".to_string(),
        )
        .into())
    }
}

// ── helpers ───────────────────────────────────────────────────────────

/// Resolve the `~/.deepagents/` home directory from `DEEPAGENTS_HOME` or the
/// OS home dir. Returns `None` if neither is available.
fn deepagents_home_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DEEPAGENTS_HOME") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var("HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from).map(|mut p| {
        p.push(".deepagents");
        p
    })
}

/// Current time as unix seconds, without pulling in a chrono/tokio dep.
fn current_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_server_config_serde() {
        let mut env = HashMap::new();
        env.insert("API_KEY".to_string(), "secret".to_string());
        let cfg = McpServerConfig {
            name: "fs".to_string(),
            transport: McpTransport::Stdio,
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
            url: None,
            env,
            trust: TrustLevel::Trusted,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(json.contains("\"name\":\"fs\""));
        assert!(json.contains("\"transport\":\"stdio\""));
        assert!(json.contains("\"trust\":\"trusted\""));

        let back: McpServerConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.name, "fs");
        assert_eq!(back.transport, McpTransport::Stdio);
        assert_eq!(back.trust, TrustLevel::Trusted);
        assert_eq!(back.args.len(), 2);
        assert_eq!(back.env.get("API_KEY").map(String::as_str), Some("secret"));
    }

    #[test]
    fn test_mcp_config_from_json() {
        let json = r#"{
            "servers": [
                {
                    "name": "fs",
                    "transport": "stdio",
                    "command": "npx",
                    "args": ["-y", "@mcp/server-fs"],
                    "trust": "trusted"
                },
                {
                    "name": "remote",
                    "transport": "http",
                    "url": "https://example.com/mcp",
                    "trust": "pending"
                }
            ],
            "reject_list": ["evil"],
            "allow_list": ["fs"]
        }"#;
        let cfg = McpConfig::from_json(json).expect("parse");
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.servers[0].name, "fs");
        assert_eq!(cfg.servers[0].transport, McpTransport::Stdio);
        assert_eq!(cfg.servers[1].transport, McpTransport::Http);
        assert_eq!(cfg.servers[1].url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(cfg.servers[1].trust, TrustLevel::Pending);
        assert_eq!(cfg.reject_list, vec!["evil"]);
        assert_eq!(cfg.allow_list, vec!["fs"]);

        // Round-trip through to_json.
        let out = cfg.to_json().expect("serialize");
        let again = McpConfig::from_json(&out).expect("reparse");
        assert_eq!(again.servers.len(), 2);
    }

    #[test]
    fn test_mcp_config_reject_wins() {
        let mut cfg = McpConfig::new();
        cfg.add_server(McpServerConfig {
            name: "evil".to_string(),
            transport: McpTransport::Stdio,
            command: Some("evil".to_string()),
            args: vec![],
            url: None,
            env: HashMap::new(),
            trust: TrustLevel::Untrusted,
        });
        cfg.reject_list.push("evil".to_string());
        // reject wins even if also in allow_list
        cfg.allow_list.push("evil".to_string());
        assert!(!cfg.is_server_allowed("evil"));
    }

    #[test]
    fn test_mcp_config_allow_list() {
        let mut cfg = McpConfig::new();
        cfg.add_server(McpServerConfig {
            name: "fs".to_string(),
            transport: McpTransport::Stdio,
            command: Some("npx".to_string()),
            args: vec![],
            url: None,
            env: HashMap::new(),
            trust: TrustLevel::Trusted,
        });
        cfg.allow_list.push("fs".to_string());

        // fs is allowed (in allow_list)
        assert!(cfg.is_server_allowed("fs"));
        // other is not in allow_list → denied
        assert!(!cfg.is_server_allowed("other"));

        // resolve_server works
        let resolved = cfg.resolve_server("fs").expect("found");
        assert_eq!(resolved.name, "fs");
        assert!(cfg.resolve_server("missing").is_none());
    }

    #[test]
    fn test_token_cache_store_load() {
        let tmp = std::env::temp_dir().join(format!(
            "da-mcp-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let cache = TokenCache::new(tmp.clone());
        let entry = TokenEntry {
            server_name: "github".to_string(),
            access_token: "abc123".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(i64::MAX),
        };
        cache.store(entry.clone()).expect("store");
        let loaded = cache.load("github").expect("load").expect("present");
        assert_eq!(loaded.server_name, entry.server_name);
        assert_eq!(loaded.access_token, entry.access_token);
        assert_eq!(loaded.refresh_token, entry.refresh_token);
        assert!(!TokenCache::is_expired(&loaded));

        // remove + load returns None
        cache.remove("github").expect("remove");
        assert!(cache.load("github").expect("load after remove").is_none());

        // cleanup
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn test_expand_env() {
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/root".to_string());
        env.insert("EMPTY".to_string(), String::new());

        assert_eq!(expand_env_with("${HOME}/sub", &env), "/root/sub");
        assert_eq!(expand_env_with("${MISSING:-fallback}", &env), "fallback");
        assert_eq!(expand_env_with("${EMPTY:-def}", &env), "def");
        assert_eq!(expand_env_with("plain text", &env), "plain text");
        assert_eq!(expand_env_with("${MISSING}", &env), "");
        assert_eq!(
            expand_env_with("a=${HOME} b=${MISSING:-x}", &env),
            "a=/root b=x"
        );
        // No closing brace → emit literally.
        assert_eq!(expand_env_with("end ${oops", &env), "end ${oops");
    }

    #[test]
    fn test_oauth_flow_serde() {
        let cases = [
            (OAuthFlow::LoopbackRedirect, "loopback_redirect"),
            (OAuthFlow::DeviceCode, "device_code"),
            (OAuthFlow::PasteBack, "paste_back"),
        ];
        for (flow, tag) in cases {
            let json = serde_json::to_string(&flow).expect("serialize");
            assert_eq!(json, format!("\"{tag}\""));
            let back: OAuthFlow = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, flow);
        }
    }

    #[tokio::test]
    async fn test_mcp_client_connect_is_stub() {
        let mut cfg = McpConfig::new();
        cfg.add_server(McpServerConfig {
            name: "fs".to_string(),
            transport: McpTransport::Stdio,
            command: Some("npx".to_string()),
            args: vec![],
            url: None,
            env: HashMap::new(),
            trust: TrustLevel::Trusted,
        });
        let client = McpClient::new(cfg, TokenCache::new(std::env::temp_dir()));
        let err = client.connect("fs").await.expect_err("stub errors");
        let msg = err.to_string();
        assert!(msg.contains("not yet implemented"), "unexpected: {msg}");
    }

    #[test]
    fn test_mcp_client_blocked_by_trust_list() {
        let mut cfg = McpConfig::new();
        cfg.add_server(McpServerConfig {
            name: "blocked".to_string(),
            transport: McpTransport::Http,
            command: None,
            args: vec![],
            url: Some("https://blocked.example".to_string()),
            env: HashMap::new(),
            trust: TrustLevel::Untrusted,
        });
        cfg.reject_list.push("blocked".to_string());
        let client = McpClient::new(cfg, TokenCache::new(std::env::temp_dir()));

        // resolve_server still finds it, but is_server_allowed denies.
        assert!(client.config.resolve_server("blocked").is_some());
        assert!(!client.config.is_server_allowed("blocked"));
    }

    #[test]
    fn test_default_dir_contains_mcp_tokens() {
        let dir = TokenCache::default_dir();
        let s = dir.to_string_lossy();
        assert!(s.contains("mcp-tokens"), "dir: {s}");
    }

    #[test]
    fn test_token_expired_logic() {
        let past = TokenEntry {
            server_name: "s".to_string(),
            access_token: "t".to_string(),
            refresh_token: None,
            expires_at: Some(1),
        };
        assert!(TokenCache::is_expired(&past));

        let none_exp = TokenEntry {
            expires_at: None,
            ..past
        };
        assert!(!TokenCache::is_expired(&none_exp));
    }
}

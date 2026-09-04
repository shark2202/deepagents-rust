//! rusqlite+bundled, 18-channel `ResumeState` (Q17).
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` §七 for the full design specification.
//!
//! This crate provides the persistence layer for the Deep Agents "session /
//! resume" feature. A session is an 18-channel [`ResumeState`] captured at a
//! point in time, allowing an agent run to be paused and later resumed with
//! full fidelity.
//!
//! The backend is a single-file SQLite database (compiled in via the `bundled`
//! feature of [`rusqlite`]) plus a `conversation_history/` directory for
//! archived conversation transcripts (compressed JSON, organized by
//! `thread_id`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use deepagents_errors::Error as DaError;
use deepagents_errors::SessionError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SQLite table holding session metadata.
pub const SESSIONS_TABLE: &str = "sessions";

/// SQLite table holding per-channel session data.
pub const CHANNELS_TABLE: &str = "channels";

// ── SessionChannel ──────────────────────────────────────────────────────

/// All 18 resumable channels of a [`ResumeState`].
///
/// Each channel is an independently versioned slot of session state. The
/// snake-case serialization mirrors the on-disk JSON layout used by the
/// upstream Python SDK.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionChannel {
    /// Conversation messages (the transcript tail).
    Messages,
    /// Pending / in-flight tool calls.
    ToolCalls,
    /// Accumulated token + dollar cost.
    Cost,
    /// Active goal (objective string + status).
    Goal,
    /// Grading rubric for the goal.
    Rubric,
    /// Human-in-the-loop approval queue / decisions.
    Approval,
    /// Compaction state (compacted ranges, last checkpoint).
    Compaction,
    /// Effective model spec (provider, model, params).
    ModelSpec,
    /// Resolved system prompt.
    SystemPrompt,
    /// Spawned subagents (config + state).
    Subagents,
    /// Registered skills.
    Skills,
    /// MCP server registry / connection state.
    McpServers,
    /// Effective permission grants.
    Permissions,
    /// Interrupt-on event configuration.
    InterruptOn,
    /// Hook registry + last results.
    Hooks,
    /// Plugin registry + state.
    Plugins,
    /// Sandbox spec for the run.
    Sandbox,
    /// Free-form run metadata (tags, env, etc.).
    Metadata,
}

impl SessionChannel {
    /// Stable on-disk key (snake_case) for this channel.
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::ToolCalls => "tool_calls",
            Self::Cost => "cost",
            Self::Goal => "goal",
            Self::Rubric => "rubric",
            Self::Approval => "approval",
            Self::Compaction => "compaction",
            Self::ModelSpec => "model_spec",
            Self::SystemPrompt => "system_prompt",
            Self::Subagents => "subagents",
            Self::Skills => "skills",
            Self::McpServers => "mcp_servers",
            Self::Permissions => "permissions",
            Self::InterruptOn => "interrupt_on",
            Self::Hooks => "hooks",
            Self::Plugins => "plugins",
            Self::Sandbox => "sandbox",
            Self::Metadata => "metadata",
        }
    }
}

/// Parse a snake-case channel key back into a [`SessionChannel`].
fn channel_from_key(key: &str) -> Option<SessionChannel> {
    Some(match key {
        "messages" => SessionChannel::Messages,
        "tool_calls" => SessionChannel::ToolCalls,
        "cost" => SessionChannel::Cost,
        "goal" => SessionChannel::Goal,
        "rubric" => SessionChannel::Rubric,
        "approval" => SessionChannel::Approval,
        "compaction" => SessionChannel::Compaction,
        "model_spec" => SessionChannel::ModelSpec,
        "system_prompt" => SessionChannel::SystemPrompt,
        "subagents" => SessionChannel::Subagents,
        "skills" => SessionChannel::Skills,
        "mcp_servers" => SessionChannel::McpServers,
        "permissions" => SessionChannel::Permissions,
        "interrupt_on" => SessionChannel::InterruptOn,
        "hooks" => SessionChannel::Hooks,
        "plugins" => SessionChannel::Plugins,
        "sandbox" => SessionChannel::Sandbox,
        "metadata" => SessionChannel::Metadata,
        _ => return None,
    })
}

/// Return all 18 [`SessionChannel`] variants in canonical order.
pub fn all_channels() -> Vec<SessionChannel> {
    vec![
        SessionChannel::Messages,
        SessionChannel::ToolCalls,
        SessionChannel::Cost,
        SessionChannel::Goal,
        SessionChannel::Rubric,
        SessionChannel::Approval,
        SessionChannel::Compaction,
        SessionChannel::ModelSpec,
        SessionChannel::SystemPrompt,
        SessionChannel::Subagents,
        SessionChannel::Skills,
        SessionChannel::McpServers,
        SessionChannel::Permissions,
        SessionChannel::InterruptOn,
        SessionChannel::Hooks,
        SessionChannel::Plugins,
        SessionChannel::Sandbox,
        SessionChannel::Metadata,
    ]
}

// ── SessionMeta ──────────────────────────────────────────────────────────

/// Top-level metadata for a persisted session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Stable session id (ulid / uuid string).
    pub session_id: String,
    /// Conversation thread id this session belongs to.
    pub thread_id: String,
    /// Unix-epoch seconds at which the session was created.
    pub created_at: i64,
    /// Unix-epoch seconds at which the session was last updated.
    pub updated_at: i64,
    /// Effective model name for the run.
    pub model: String,
}

// ── ChannelData ─────────────────────────────────────────────────────────

/// A single channel's payload inside a [`ResumeState`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelData {
    /// Which channel this payload belongs to.
    pub channel: SessionChannel,
    /// Arbitrary JSON payload for the channel.
    pub data: serde_json::Value,
    /// Unix-epoch seconds at which the channel was last updated.
    pub updated_at: i64,
}

// ── ResumeState ─────────────────────────────────────────────────────────

/// The full resumable state of an agent run: 18 channels keyed by
/// [`SessionChannel`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeState {
    /// Session id this state belongs to.
    pub session_id: String,
    /// Conversation thread id this state belongs to.
    pub thread_id: String,
    /// The 18 (possibly sparse) channels.
    pub channels: HashMap<SessionChannel, ChannelData>,
}

impl ResumeState {
    /// Create an empty `ResumeState` for the given session + thread.
    pub fn new(session_id: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            thread_id: thread_id.into(),
            channels: HashMap::new(),
        }
    }

    /// Insert or replace a channel's payload, stamping `updated_at` to now.
    pub fn set_channel(&mut self, channel: SessionChannel, data: serde_json::Value) {
        let updated_at = unix_now();
        self.channels.insert(
            channel.clone(),
            ChannelData {
                channel,
                data,
                updated_at,
            },
        );
    }

    /// Look up a channel's payload, if present.
    pub fn get_channel(&self, channel: &SessionChannel) -> Option<&ChannelData> {
        self.channels.get(channel)
    }

    /// Number of channels currently populated.
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
}

// ── SessionStore ─────────────────────────────────────────────────────────

/// SQLite-backed persistent store for sessions + archived conversations.
///
/// Holds a single [`rusqlite::Connection`] (note: not `Send`/`Sync`) and a
/// `conversation_history/` directory path used by
/// [`archive_conversation`](Self::archive_conversation).
pub struct SessionStore {
    /// The SQLite connection. Not `Send`/`Sync`.
    pub conn: Connection,
    /// Directory under which archived conversations are stored.
    pub history_dir: PathBuf,
}

impl SessionStore {
    /// Open (or create) the SQLite database at `db_path`, initialize the
    /// schema, and ensure the `history_dir` exists.
    pub fn new(
        db_path: impl AsRef<Path>,
        history_dir: impl AsRef<Path>,
    ) -> Result<Self, DaError> {
        let conn = Connection::open(db_path.as_ref()).map_err(rusqlite_err)?;
        let history_dir = history_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&history_dir)?;
        let store = Self {
            conn,
            history_dir,
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Return `(sessions.db, conversation_history/)` under
    /// `~/.deepagents/.state/`.
    pub fn default_paths() -> Result<(PathBuf, PathBuf), DaError> {
        let home = home_dir()?;
        let state_dir = home.join(".deepagents").join(".state");
        std::fs::create_dir_all(&state_dir)?;
        let db_path = state_dir.join("sessions.db");
        let history_dir = state_dir.join("conversation_history");
        std::fs::create_dir_all(&history_dir)?;
        Ok((db_path, history_dir))
    }

    /// Create the `sessions` and `channels` tables (idempotent).
    pub fn init_schema(&self) -> Result<(), DaError> {
        self.conn
            .execute_batch(&format!(
                r#"
                CREATE TABLE IF NOT EXISTS {SESSIONS_TABLE} (
                    id          TEXT PRIMARY KEY,
                    thread_id   TEXT NOT NULL,
                    created_at  INTEGER NOT NULL,
                    updated_at  INTEGER NOT NULL,
                    model       TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS {CHANNELS_TABLE} (
                    session_id  TEXT NOT NULL,
                    channel     TEXT NOT NULL,
                    data        TEXT NOT NULL,
                    updated_at  INTEGER NOT NULL,
                    PRIMARY KEY (session_id, channel),
                    FOREIGN KEY (session_id) REFERENCES {SESSIONS_TABLE}(id)
                        ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_channels_session
                    ON {CHANNELS_TABLE}(session_id);
                CREATE INDEX IF NOT EXISTS idx_sessions_thread
                    ON {SESSIONS_TABLE}(thread_id);
                "#
            ))
            .map_err(rusqlite_err)?;
        Ok(())
    }

    /// Upsert a session's metadata and all of its channels atomically.
    pub fn save_session(
        &self,
        meta: &SessionMeta,
        state: &ResumeState,
    ) -> Result<(), DaError> {
        let tx = self.conn.unchecked_transaction().map_err(rusqlite_err)?;

        tx.execute(
            &format!(
                "INSERT INTO {SESSIONS_TABLE} (id, thread_id, created_at, updated_at, model)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    thread_id  = excluded.thread_id,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at,
                    model      = excluded.model"
            ),
            rusqlite::params![
                meta.session_id,
                meta.thread_id,
                meta.created_at,
                meta.updated_at,
                meta.model,
            ],
        )
        .map_err(rusqlite_err)?;

        // Replace all channels for this session: simplest correct semantics.
        tx.execute(
            &format!("DELETE FROM {CHANNELS_TABLE} WHERE session_id = ?1"),
            rusqlite::params![meta.session_id],
        )
        .map_err(rusqlite_err)?;

        let mut stmt = tx
            .prepare(&format!(
                "INSERT INTO {CHANNELS_TABLE} (session_id, channel, data, updated_at)
                 VALUES (?1, ?2, ?3, ?4)"
            ))
            .map_err(rusqlite_err)?;

        for (channel, cd) in &state.channels {
            let data_str = serde_json::to_string(&cd.data)?;
            stmt.execute(rusqlite::params![
                meta.session_id,
                channel.as_key(),
                data_str,
                cd.updated_at,
            ])
            .map_err(rusqlite_err)?;
        }
        drop(stmt);

        tx.commit().map_err(rusqlite_err)?;
        Ok(())
    }

    /// Load a session's metadata + all populated channels.
    ///
    /// Returns `Ok(None)` if no session with `session_id` exists.
    pub fn load_session(
        &self,
        session_id: &str,
    ) -> Result<Option<(SessionMeta, ResumeState)>, DaError> {
        let meta: Option<(String, String, i64, i64, String)> = self
            .conn
            .query_row(
                &format!(
                    "SELECT id, thread_id, created_at, updated_at, model
                     FROM {SESSIONS_TABLE} WHERE id = ?1"
                ),
                rusqlite::params![session_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(rusqlite_err(other)),
            })?;

        let Some((sid, tid, created_at, updated_at, model)) = meta else {
            return Ok(None);
        };

        let mut channels: HashMap<SessionChannel, ChannelData> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT channel, data, updated_at
                     FROM {CHANNELS_TABLE} WHERE session_id = ?1"
                ))
                .map_err(rusqlite_err)?;
            let rows = stmt
                .query_map(rusqlite::params![session_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(rusqlite_err)?;
            for row in rows {
                let (channel_key, data_str, updated_at) = row.map_err(rusqlite_err)?;
                let channel = channel_from_key(&channel_key).ok_or_else(|| {
                    SessionError::Deserialize(format!("unknown channel key: {channel_key}"))
                })?;
                let data: serde_json::Value = serde_json::from_str(&data_str).map_err(|e| {
                    SessionError::Deserialize(format!("channel {channel_key}: {e}"))
                })?;
                channels.insert(
                    channel.clone(),
                    ChannelData {
                        channel,
                        data,
                        updated_at,
                    },
                );
            }
        }

        let meta = SessionMeta {
            session_id: sid.clone(),
            thread_id: tid.clone(),
            created_at,
            updated_at,
            model,
        };
        let state = ResumeState {
            session_id: sid,
            thread_id: tid,
            channels,
        };
        Ok(Some((meta, state)))
    }

    /// List metadata for every persisted session, ordered by `created_at`.
    pub fn list_sessions(&self) -> Result<Vec<SessionMeta>, DaError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT id, thread_id, created_at, updated_at, model
                 FROM {SESSIONS_TABLE}
                 ORDER BY created_at ASC"
            ))
            .map_err(rusqlite_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SessionMeta {
                    session_id: r.get::<_, String>(0)?,
                    thread_id: r.get::<_, String>(1)?,
                    created_at: r.get::<_, i64>(2)?,
                    updated_at: r.get::<_, i64>(3)?,
                    model: r.get::<_, String>(4)?,
                })
            })
            .map_err(rusqlite_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(rusqlite_err)?);
        }
        Ok(out)
    }

    /// Delete a session and all of its channels.
    pub fn delete_session(&self, session_id: &str) -> Result<(), DaError> {
        let tx = self.conn.unchecked_transaction().map_err(rusqlite_err)?;
        tx.execute(
            &format!("DELETE FROM {CHANNELS_TABLE} WHERE session_id = ?1"),
            rusqlite::params![session_id],
        )
        .map_err(rusqlite_err)?;
        tx.execute(
            &format!("DELETE FROM {SESSIONS_TABLE} WHERE id = ?1"),
            rusqlite::params![session_id],
        )
        .map_err(rusqlite_err)?;
        tx.commit().map_err(rusqlite_err)?;
        Ok(())
    }

    /// Serialize `messages` to JSON and write them to
    /// `history_dir/{thread_id}.json`.
    pub fn archive_conversation(
        &self,
        thread_id: &str,
        messages: &serde_json::Value,
    ) -> Result<(), DaError> {
        let path = self.history_dir.join(format!("{thread_id}.json"));
        let bytes = serde_json::to_vec_pretty(messages)?;
        std::fs::write(&path, bytes)?;
        Ok(())
    }

    /// Read an archived conversation transcript for `thread_id`, if present.
    pub fn load_archived_conversation(
        &self,
        thread_id: &str,
    ) -> Result<Option<serde_json::Value>, DaError> {
        let path = self.history_dir.join(format!("{thread_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        Ok(Some(value))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Map a [`rusqlite::Error`] into a [`SessionError::Database`].
fn rusqlite_err(e: rusqlite::Error) -> DaError {
    DaError::Session(SessionError::Database(e.to_string()))
}

/// Current unix-epoch seconds.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolve the user's home directory without depending on the `dirs` crate.
fn home_dir() -> Result<PathBuf, DaError> {
    if let Ok(h) = std::env::var("HOME")
        && !h.is_empty()
    {
        return Ok(PathBuf::from(h));
    }
    // Fallback for non-Unix. Last resort: current dir.
    Ok(PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique counter so each test gets a distinct temp subdir / db file.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_tmpdir(prefix: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut p = std::env::temp_dir();
        p.push(format!("{prefix}_{}", n));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn test_session_channel_serde() {
        // snake_case round-trip on every variant.
        for ch in all_channels() {
            let s = serde_json::to_string(&ch).unwrap();
            let back: SessionChannel = serde_json::from_str(&s).unwrap();
            assert_eq!(ch, back, "round-trip mismatch for {s}");
            assert!(
                s.contains('"'),
                "expected a JSON string for {ch:?}, got {s}"
            );
            // snake_case should never contain uppercase letters inside quotes.
            let inner = s.trim_matches('"');
            assert!(
                !inner.chars().any(|c| c.is_ascii_uppercase()),
                "channel key {inner:?} must be snake_case (no uppercase)"
            );
        }
        assert_eq!(all_channels().len(), 18);
    }

    #[test]
    fn test_resume_state_set_get() {
        let mut rs = ResumeState::new("sess-1", "thr-1");
        assert!(rs.get_channel(&SessionChannel::Goal).is_none());
        rs.set_channel(
            SessionChannel::Goal,
            serde_json::json!({ "objective": "ship it" }),
        );
        let got = rs.get_channel(&SessionChannel::Goal).unwrap();
        assert_eq!(got.channel, SessionChannel::Goal);
        assert_eq!(got.data["objective"], "ship it");
    }

    #[test]
    fn test_resume_state_channel_count() {
        let mut rs = ResumeState::new("sess-2", "thr-2");
        assert_eq!(rs.channel_count(), 0);
        rs.set_channel(SessionChannel::Messages, serde_json::json!([]));
        rs.set_channel(SessionChannel::Cost, serde_json::json!({"usd": 0.42}));
        assert_eq!(rs.channel_count(), 2);
        // replacing a channel shouldn't bump the count.
        rs.set_channel(SessionChannel::Messages, serde_json::json!([1]));
        assert_eq!(rs.channel_count(), 2);
    }

    #[test]
    fn test_session_store_create() {
        let dir = unique_tmpdir("ss_create");
        let db = dir.join("sessions.db");
        let hist = dir.join("conversation_history");
        let store = SessionStore::new(&db, &hist).unwrap();
        // history dir exists
        assert!(hist.exists());
        // schema tables exist: a no-op re-init must succeed & be idempotent
        store.init_schema().unwrap();
        store.init_schema().unwrap();
        // tables are queryable (a SELECT count(*) must return 0 on each).
        let n: i64 = store
            .conn
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = store
            .conn
            .query_row("SELECT count(*) FROM channels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_session_store_save_load() {
        let dir = unique_tmpdir("ss_save_load");
        let store = SessionStore::new(dir.join("s.db"), dir.join("hist")).unwrap();

        let meta = SessionMeta {
            session_id: "s-1".into(),
            thread_id: "t-1".into(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_010,
            model: "gpt-test".into(),
        };
        let mut state = ResumeState::new("s-1", "t-1");
        state.set_channel(SessionChannel::Messages, serde_json::json!([{"role": "user"}]));
        state.set_channel(
            SessionChannel::Cost,
            serde_json::json!({"tokens": 1234, "usd": 0.05}),
        );

        store.save_session(&meta, &state).unwrap();

        let loaded = store.load_session("s-1").unwrap().expect("session must load");
        assert_eq!(loaded.0.session_id, "s-1");
        assert_eq!(loaded.0.thread_id, "t-1");
        assert_eq!(loaded.0.model, "gpt-test");
        assert_eq!(loaded.1.session_id, "s-1");
        assert_eq!(loaded.1.thread_id, "t-1");
        assert_eq!(loaded.1.channel_count(), 2);

        let msgs = loaded.1.get_channel(&SessionChannel::Messages).unwrap();
        assert_eq!(msgs.data[0]["role"], "user");
        let cost = loaded.1.get_channel(&SessionChannel::Cost).unwrap();
        assert_eq!(cost.data["tokens"], 1234);

        // load missing -> None
        assert!(store.load_session("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn test_session_store_list() {
        let dir = unique_tmpdir("ss_list");
        let store = SessionStore::new(dir.join("s.db"), dir.join("hist")).unwrap();

        for (i, sid) in ["a", "b"].iter().enumerate() {
            let meta = SessionMeta {
                session_id: (*sid).into(),
                thread_id: format!("thr-{sid}"),
                created_at: 1_700_000_000 + i as i64,
                updated_at: 1_700_000_005 + i as i64,
                model: "m".into(),
            };
            let mut state = ResumeState::new(*sid, &format!("thr-{sid}"));
            state.set_channel(SessionChannel::Goal, serde_json::json!({"i": i}));
            store.save_session(&meta, &state).unwrap();
        }

        let list = store.list_sessions().unwrap();
        assert_eq!(list.len(), 2);
        // ordered by created_at asc
        assert_eq!(list[0].session_id, "a");
        assert_eq!(list[1].session_id, "b");
    }

    #[test]
    fn test_session_store_delete() {
        let dir = unique_tmpdir("ss_delete");
        let store = SessionStore::new(dir.join("s.db"), dir.join("hist")).unwrap();

        let meta = SessionMeta {
            session_id: "del-1".into(),
            thread_id: "t".into(),
            created_at: 1,
            updated_at: 2,
            model: "m".into(),
        };
        let mut state = ResumeState::new("del-1", "t");
        state.set_channel(SessionChannel::Metadata, serde_json::json!({"k": "v"}));
        store.save_session(&meta, &state).unwrap();
        assert!(store.load_session("del-1").unwrap().is_some());

        store.delete_session("del-1").unwrap();
        assert!(store.load_session("del-1").unwrap().is_none());
        // deleting again is a no-op success
        store.delete_session("del-1").unwrap();
    }

    #[test]
    fn test_archive_conversation() {
        let dir = unique_tmpdir("ss_archive");
        let store = SessionStore::new(dir.join("s.db"), dir.join("hist")).unwrap();

        let msgs = serde_json::json!([
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": "hello" }
        ]);
        store.archive_conversation("thr-7", &msgs).unwrap();

        let loaded = store
            .load_archived_conversation("thr-7")
            .unwrap()
            .expect("archive must exist");
        assert_eq!(loaded, msgs);
        assert_eq!(loaded[0]["content"], "hi");

        // missing thread -> None
        assert!(store.load_archived_conversation("nope").unwrap().is_none());
    }
}

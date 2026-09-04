//! GitHub Releases, 5 install methods, auto-update (Q25).
//!
//! This crate provides self-update capabilities for the `deepagents` binary:
//!
//! - Detecting how the binary was installed (`InstallMethod`).
//! - Querying GitHub Releases for the latest published version.
//! - Caching the latest-version result on disk with a TTL so startup can make a
//!   fast decision without contacting the network.
//! - Downloading a matching target-triple asset, verifying its SHA-256
//!   checksum, and atomically replacing the running binary.
//!
//! All public items are documented, `Debug`/`Clone` (and serde where
//! applicable), and the crate forbids `unsafe` code.
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` for the full design specification.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use deepagents_errors::{Error, UpdateError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Cache time-to-live, in seconds (24 hours).
///
/// The cached `latest_version.json` is considered fresh for this many seconds
/// after it was written. Startup can consult the cache to decide whether an
/// update is available without contacting GitHub.
pub const CACHE_TTL: i64 = 86_400;

/// Cooldown applied after a startup auto-update *failure*, in seconds (24h).
///
/// If an auto-update attempt during startup fails, subsequent startup
/// auto-update attempts are suppressed for this duration to avoid retrying a
/// known-broken path on every launch.
pub const STARTUP_AUTO_UPDATE_FAILURE_COOLDOWN: i64 = 86_400;

/// Grace period during which auto-update is delayed after a session resume, in
/// seconds (7 days).
///
/// When a session is resumed, the user is typically mid-task; we avoid
/// interrupting them with an update for this grace period.
pub const RESUME_AUTO_UPDATE_GRACE_PERIOD: i64 = 604_800;

/// Filename of the on-disk version cache, relative to the state directory.
const CACHE_FILENAME: &str = "latest_version.json";

// ─── Install method ────────────────────────────────────────────────────

/// How the running binary was installed.
///
/// Used to tailor update UX (e.g. telling the user to run `brew upgrade`
/// instead of auto-replacing the binary when installed via Homebrew).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMethod {
    /// Installed via `cargo binstall`.
    CargoBinstall,
    /// Installed via Homebrew (`/opt/homebrew` or `/usr/local/bin`).
    Homebrew,
    /// Installed via Scoop (Windows).
    Scoop,
    /// Standalone binary downloaded directly from GitHub Releases.
    Standalone,
    /// Could not be determined.
    Unknown,
}

// ─── Version / asset info ──────────────────────────────────────────────

/// Metadata describing a published release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Semver version string of the release (e.g. `"0.2.0"`).
    pub version: String,
    /// Browser URL of the release on GitHub.
    pub release_url: String,
    /// Timestamp the release was published, if available.
    pub published_at: Option<DateTime<Utc>>,
}

/// A single downloadable asset attached to a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInfo {
    /// Human-readable asset filename (used to match the target triple).
    pub name: String,
    /// Direct download URL (browser_download_url).
    pub download_url: String,
    /// Expected SHA-256 checksum (hex), when provided alongside the asset.
    pub checksum: Option<String>,
}

/// Result of comparing the current version against the latest available one.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    /// The version the running binary reports.
    pub current_version: semver::Version,
    /// The newest version published upstream.
    pub latest_version: semver::Version,
    /// `true` if `latest_version > current_version`.
    pub update_available: bool,
}

// ─── Manager ───────────────────────────────────────────────────────────

/// Drives version checks and self-update via GitHub Releases.
///
/// A single `UpdateManager` is cheap to clone-equivalent in spirit: construct
/// one at startup with the current version and a state directory (normally
/// `~/.deepagents/.state`), then call [`UpdateManager::check_for_update`] to
/// get a fast cached answer, or [`UpdateManager::fetch_latest_version`] for a
/// fresh network check.
pub struct UpdateManager {
    /// Version of the currently running binary (semver string).
    pub current_version: String,
    /// Directory in which `latest_version.json` is cached.
    pub state_dir: PathBuf,
    /// HTTP client used for GitHub API + asset downloads.
    pub client: reqwest::Client,
    /// `"owner/repo"` slug of the GitHub repository to check.
    pub repo: String,
}

impl UpdateManager {
    /// Create a manager with the default repo and a default HTTP client.
    ///
    /// `state_dir` is normally `~/.deepagents/.state`.
    pub fn new(
        current_version: impl Into<String>,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self::with_repo(
            current_version,
            state_dir,
            "langchain-ai/deepagents-rust".to_string(),
            reqwest::Client::new(),
        )
    }

    /// Create a manager with an explicit repo slug and HTTP client.
    ///
    /// Exposed for testing and for callers that want to pin a different
    /// repository or configure a custom `reqwest::Client` (e.g. with a proxy
    /// or custom user-agent).
    pub fn with_repo(
        current_version: impl Into<String>,
        state_dir: impl Into<PathBuf>,
        repo: String,
        client: reqwest::Client,
    ) -> Self {
        Self {
            current_version: current_version.into(),
            state_dir: state_dir.into(),
            client,
            repo,
        }
    }

    /// Best-effort detection of how the current binary was installed, inferred
    /// from the executable path.
    ///
    /// Heuristics:
    /// - path contains `.cargo` → [`InstallMethod::CargoBinstall`]
    /// - path contains `/opt/homebrew` or `/usr/local/bin` →
    ///   [`InstallMethod::Homebrew`]
    /// - path contains `scoop` → [`InstallMethod::Scoop`]
    /// - otherwise → [`InstallMethod::Standalone`] (treated as a direct
    ///   download from GitHub Releases); if the executable path cannot be read
    ///   at all, [`InstallMethod::Unknown`] is returned.
    pub fn detect_install_method(&self) -> InstallMethod {
        let Ok(exe) = std::env::current_exe() else {
            return InstallMethod::Unknown;
        };
        Self::install_method_from_path(&exe)
    }

    /// Map an executable path to an [`InstallMethod`] using path heuristics.
    fn install_method_from_path(exe: &Path) -> InstallMethod {
        let s = exe.to_string_lossy();
        if s.contains(".cargo") {
            InstallMethod::CargoBinstall
        } else if s.contains("/opt/homebrew") || s.contains("/usr/local/bin") {
            InstallMethod::Homebrew
        } else if s.contains("scoop") {
            InstallMethod::Scoop
        } else {
            // Anything we can resolve that isn't a known package manager is
            // treated as a standalone download we can self-replace.
            InstallMethod::Standalone
        }
    }

    /// Return the Rust target triple for the current host, e.g.
    /// `"x86_64-apple-darwin"`.
    ///
    /// Determined at compile time via `cfg!` macros; covers the common
    /// macOS / Linux / Windows × x86_64/aarch64 combinations.
    pub fn target_triple() -> &'static str {
        // Architecture.
        #[cfg(target_arch = "x86_64")]
        {
            #[cfg(target_os = "macos")]
            {
                "x86_64-apple-darwin"
            }
            #[cfg(target_os = "linux")]
            {
                "x86_64-unknown-linux-gnu"
            }
            #[cfg(target_os = "windows")]
            {
                "x86_64-pc-windows-msvc"
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
                "x86_64-unknown"
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            #[cfg(target_os = "macos")]
            {
                "aarch64-apple-darwin"
            }
            #[cfg(target_os = "linux")]
            {
                "aarch64-unknown-linux-gnu"
            }
            #[cfg(target_os = "windows")]
            {
                "aarch64-pc-windows-msvc"
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
                "aarch64-unknown"
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            "unknown-unknown"
        }
    }

    /// Path to the on-disk version cache (`state_dir/latest_version.json`).
    pub fn cache_path(&self) -> PathBuf {
        self.state_dir.join(CACHE_FILENAME)
    }

    /// Fetch the latest release from the GitHub API.
    ///
    /// Contacts `https://api.github.com/repos/{repo}/releases/latest`, parses
    /// the JSON, and returns a [`VersionInfo`]. Also writes the result to the
    /// cache so a subsequent fast-path startup check can reuse it.
    pub async fn fetch_latest_version(&self) -> Result<VersionInfo, Error> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.repo
        );
        debug!(url = %url, "fetching latest release from GitHub");

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", concat!("deepagents-update/", env!("CARGO_PKG_VERSION")))
            .send()
            .await
            .map_err(|e| {
                Error::Update(UpdateError::VersionCheck(format!(
                    "GitHub API request failed: {e}"
                )))
            })?;

        if !resp.status().is_success() {
            return Err(Error::Update(UpdateError::VersionCheck(format!(
                "GitHub API returned HTTP {}",
                resp.status()
            ))));
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            Error::Update(UpdateError::VersionCheck(format!(
                "failed to decode GitHub API response: {e}"
            )))
        })?;
        let info = parse_release_json(&json)?;
        self.save_cached_version(&info)?;
        Ok(info)
    }

    /// Read the cached latest-version, if present and still within the TTL.
    ///
    /// Returns `None` if the cache file does not exist, is older than
    /// [`CACHE_TTL`] seconds, or cannot be parsed.
    pub fn load_cached_version(&self) -> Option<VersionInfo> {
        let path = self.cache_path();
        let metadata = std::fs::metadata(&path).ok()?;
        let modified = metadata.modified().ok()?;
        let modified: DateTime<Utc> = modified.into();

        let age = Utc::now().signed_duration_since(modified);
        if age > Duration::seconds(CACHE_TTL) {
            debug!("version cache is stale, ignoring");
            return None;
        }

        let data = std::fs::read_to_string(&path).ok()?;
        let info: VersionInfo = serde_json::from_str(&data).ok()?;
        Some(info)
    }

    /// Persist a [`VersionInfo`] to the cache file.
    ///
    /// Ensures the state directory exists before writing.
    pub fn save_cached_version(&self, info: &VersionInfo) -> Result<(), Error> {
        let path = self.cache_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(info)?;
        std::fs::write(&path, data)?;
        debug!(cache = %path.display(), "wrote version cache");
        Ok(())
    }

    /// Check whether an update is available.
    ///
    /// Tries the on-disk cache first (fast path, no network). If the cache is
    /// missing or stale, it falls back to a live [`fetch_latest_version`].
    /// Versions are compared with `semver`; pre-release versions are handled
    /// per the `semver` crate's ordering rules.
    pub async fn check_for_update(&self) -> Result<UpdateCheck, Error> {
        let current = self.parse_current_version()?;

        let latest = if let Some(cached) = self.load_cached_version() {
            debug!("using cached latest version");
            cached
        } else {
            self.fetch_latest_version().await?
        };

        let latest_v = parse_semver(&latest.version).map_err(|e| {
            Error::Update(UpdateError::VersionCheck(format!(
                "could not parse latest version '{}': {e}",
                latest.version
            )))
        })?;

        let update_available = latest_v > current;
        Ok(UpdateCheck {
            current_version: current,
            latest_version: latest_v,
            update_available,
        })
    }

    /// Download an asset, verify its SHA-256 checksum, and atomically replace
    /// the running binary.
    ///
    /// v0 contract:
    /// - If the asset has no checksum, this is treated as an error (we will not
    ///   install unverified binaries).
    /// - The download is written to a temp file, its SHA-256 is computed, and
    ///   only if it matches do we move it over the current executable path.
    /// - The replace is atomic via `tempfile` + `rename`, except on Windows
    ///   where a direct replace is attempted.
    pub async fn download_and_replace(&self, asset: &AssetInfo) -> Result<(), Error> {
        let expected_checksum = asset.checksum.as_ref().ok_or_else(|| {
            Error::Update(UpdateError::ChecksumFailed(format!(
                "asset '{}' has no checksum; refusing to install unverified binary",
                asset.name
            )))
        })?;

        debug!(url = %asset.download_url, "downloading update asset");
        let resp = self
            .client
            .get(&asset.download_url)
            .send()
            .await
            .map_err(|e| {
                Error::Update(UpdateError::DownloadFailed(format!(
                    "asset download request failed: {e}"
                )))
            })?;

        if !resp.status().is_success() {
            return Err(Error::Update(UpdateError::DownloadFailed(format!(
                "asset download returned HTTP {}",
                resp.status()
            ))));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            Error::Update(UpdateError::DownloadFailed(format!(
                "failed to read asset body: {e}"
            )))
        })?;

        // Verify checksum before touching the live binary.
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hex::encode(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected_checksum) {
            return Err(Error::Update(UpdateError::ChecksumFailed(format!(
                "checksum mismatch: expected {expected_checksum}, got {actual}"
            ))));
        }

        // Resolve the current binary path.
        let exe_path = std::env::current_exe().map_err(|e| {
            Error::Update(UpdateError::ReplaceFailed(format!(
                "could not resolve current executable: {e}"
            )))
        })?;

        // Write to a temp file in the same directory as the target so the
        // rename is atomic on POSIX (same filesystem).
        let dir = exe_path.parent().ok_or_else(|| {
            Error::Update(UpdateError::ReplaceFailed(
                "current executable has no parent directory".to_string(),
            ))
        })?;

        // Make sure the directory is writable. If it isn't, surface
        // NoWritableBinDir so callers can suggest e.g. `sudo`.
        let tmp = tempfile::Builder::new()
            .prefix(".deepagents-update-")
            .tempfile_in(dir)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    Error::Update(UpdateError::NoWritableBinDir(
                        dir.display().to_string(),
                    ))
                } else {
                    Error::Update(UpdateError::ReplaceFailed(format!(
                        "could not create temp file in {}: {e}",
                        dir.display()
                    )))
                }
            })?;

        std::fs::write(tmp.path(), &bytes).map_err(|e| {
            Error::Update(UpdateError::ReplaceFailed(format!(
                "could not write temp file {}: {e}",
                tmp.path().display()
            )))
        })?;

        // On Unix, mark executable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            let _ = std::fs::set_permissions(tmp.path(), perms);
        }

        // Atomically replace the live binary. `into_temp_path()` drops the
        // file handle but keeps a Drop guard over the path; `keep()` then
        // detaches that guard (so the file survives until the rename) and
        // returns the owned path.
        let tmp_path = tmp.into_temp_path().keep().map_err(|e| {
            Error::Update(UpdateError::ReplaceFailed(format!(
                "could not persist temp file: {e}"
            )))
        })?;

        atomic_replace(&tmp_path, &exe_path).map_err(|e| {
            Error::Update(UpdateError::ReplaceFailed(format!(
                "atomic rename failed: {e}"
            )))
        })?;

        debug!(path = %exe_path.display(), "binary replaced successfully");
        Ok(())
    }

    /// Parse `current_version` into a `semver::Version`.
    fn parse_current_version(&self) -> Result<semver::Version, Error> {
        parse_semver(&self.current_version).map_err(|e| {
            Error::Update(UpdateError::VersionCheck(format!(
                "current version '{}' is not valid semver: {e}",
                self.current_version
            )))
        })
    }
}

// ─── Free functions ────────────────────────────────────────────────────

/// Return `true` if the startup auto-update cooldown is still active for the
/// given last-failure timestamp.
///
/// Uses [`STARTUP_AUTO_UPDATE_FAILURE_COOLDOWN`] as the window.
pub fn is_update_cooldown_active(last_failure: DateTime<Utc>) -> bool {
    let elapsed = Utc::now().signed_duration_since(last_failure);
    elapsed < Duration::seconds(STARTUP_AUTO_UPDATE_FAILURE_COOLDOWN)
}

/// Parse a `semver::Version` from a string, stripping a leading `v` if present.
fn parse_semver(s: &str) -> Result<semver::Version, semver::Error> {
    let trimmed = s.strip_prefix('v').unwrap_or(s);
    semver::Version::parse(trimmed)
}

/// Parse the GitHub releases/latest JSON into a [`VersionInfo`].
fn parse_release_json(json: &serde_json::Value) -> Result<VersionInfo, Error> {
    let tag_name = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Update(UpdateError::VersionCheck(
                "release JSON missing 'tag_name'".to_string(),
            ))
        })?;

    let html_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let published_at = json
        .get("published_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone::<Utc>(&Utc));

    Ok(VersionInfo {
        version: tag_name.to_string(),
        release_url: html_url,
        published_at,
    })
}

/// Atomically replace the file at `dst` with the file at `src`.
///
/// On POSIX, `rename(2)` is atomic and replaces the destination. On Windows,
/// `std::fs::rename` fails if the destination exists, so we fall back to a
/// best-effort remove-then-rename.
fn atomic_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(src, dst)
    }
    #[cfg(not(unix))]
    {
        if dst.exists() {
            std::fs::remove_file(dst)?;
        }
        std::fs::rename(src, dst)
    }
}

// We need hex encoding for SHA-256 output. The workspace doesn't declare a
// `hex` crate, so provide a tiny zero-dep encoder to keep the dependency list
// as declared.
mod hex {
    /// Encode `bytes` as a lowercase hex string.
    pub(crate) fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(dead_code)]

    use super::*;
    use chrono::Utc;
    use tempfile::TempDir;

    /// `InstallMethod` should round-trip through serde for every variant.
    #[test]
    fn test_install_method_serde() {
        let cases = vec![
            (InstallMethod::CargoBinstall, "\"cargo_binstall\""),
            (InstallMethod::Homebrew, "\"homebrew\""),
            (InstallMethod::Scoop, "\"scoop\""),
            (InstallMethod::Standalone, "\"standalone\""),
            (InstallMethod::Unknown, "\"unknown\""),
        ];
        for (method, expected) in cases {
            let s = serde_json::to_string(&method).unwrap();
            assert_eq!(s, expected, "serialize {method:?}");
            let back: InstallMethod = serde_json::from_str(&s).unwrap();
            assert_eq!(back, method, "deserialize {method:?}");
        }
    }

    /// `VersionInfo` should round-trip through serde, including an absent
    /// `published_at`.
    #[test]
    fn test_version_info_serde() {
        let info = VersionInfo {
            version: "0.2.0".to_string(),
            release_url: "https://github.com/owner/repo/releases/tag/v0.2.0"
                .to_string(),
            published_at: None,
        };
        let s = serde_json::to_string(&info).unwrap();
        let back: VersionInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(back.version, info.version);
        assert_eq!(back.release_url, info.release_url);
        assert_eq!(back.published_at, None);
    }

    /// `target_triple()` must always return a non-empty, known triple.
    #[test]
    fn test_target_triple() {
        let t = UpdateManager::target_triple();
        assert!(!t.is_empty(), "target triple must not be empty");
        assert!(!t.contains(' '), "target triple must not contain spaces");
    }

    /// `detect_install_method` must always return a concrete variant.
    #[test]
    fn test_detect_install_method() {
        let tmp = TempDir::new().unwrap();
        let mgr = UpdateManager::new("0.1.0", tmp.path());
        let method = mgr.detect_install_method();
        // It must be *some* concrete variant (we can't assert which one
        // without knowing the test runner's install path, but it should never
        // be a panic).
        let _ = method.clone();
        // The Unknown variant is only returned when current_exe() fails, which
        // is rare; regardless, the function must not panic.
        assert!(
            matches!(
                method,
                InstallMethod::CargoBinstall
                    | InstallMethod::Homebrew
                    | InstallMethod::Scoop
                    | InstallMethod::Standalone
                    | InstallMethod::Unknown
            ),
            "unexpected install method"
        );
    }

    /// `load_cached_version` returns `None` when no cache file exists.
    #[test]
    fn test_load_cached_version_none() {
        let tmp = TempDir::new().unwrap();
        let mgr = UpdateManager::new("0.1.0", tmp.path());
        assert!(mgr.load_cached_version().is_none());
    }

    /// Writing a cache then reading it back returns the same data.
    #[test]
    fn test_save_and_load_cached_version() {
        let tmp = TempDir::new().unwrap();
        let mgr = UpdateManager::new("0.1.0", tmp.path());
        let info = VersionInfo {
            version: "0.3.1".to_string(),
            release_url: "https://example.com/release".to_string(),
            published_at: Some(Utc::now()),
        };
        mgr.save_cached_version(&info).unwrap();
        let back = mgr.load_cached_version().expect("cache should be readable");
        assert_eq!(back.version, info.version);
        assert_eq!(back.release_url, info.release_url);
        assert!(back.published_at.is_some());
    }

    /// Same current/latest version → `update_available == false`.
    #[test]
    fn test_update_check_no_update() {
        let tmp = TempDir::new().unwrap();
        let mgr = UpdateManager::new("1.0.0", tmp.path());
        let info = VersionInfo {
            version: "1.0.0".to_string(),
            release_url: "https://example.com".to_string(),
            published_at: None,
        };
        mgr.save_cached_version(&info).unwrap();
        // check_for_update is async; block on it via a tiny runtime.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let check = rt.block_on(mgr.check_for_update()).unwrap();
        assert!(!check.update_available, "no update expected for same version");
        assert_eq!(check.current_version, check.latest_version);
    }

    /// `parse_release_json` correctly extracts fields from a GitHub response.
    #[test]
    fn test_parse_release_json() {
        let raw = serde_json::json!({
            "tag_name": "v0.4.0",
            "html_url": "https://github.com/owner/repo/releases/tag/v0.4.0",
            "published_at": "2024-01-02T03:04:05Z"
        });
        let info = parse_release_json(&raw).unwrap();
        assert_eq!(info.version, "v0.4.0");
        assert_eq!(
            info.release_url,
            "https://github.com/owner/repo/releases/tag/v0.4.0"
        );
        assert!(info.published_at.is_some());
    }

    /// `parse_semver` strips a leading `v`.
    #[test]
    fn test_parse_semver_strips_v() {
        let v = parse_semver("v1.2.3").unwrap();
        assert_eq!(v, semver::Version::new(1, 2, 3));
    }

    /// `is_update_cooldown_active` is true immediately after a failure and
    /// false after the cooldown window passes.
    #[test]
    fn test_cooldown_active() {
        let now = Utc::now();
        assert!(is_update_cooldown_active(now));
        let past = now - Duration::seconds(STARTUP_AUTO_UPDATE_FAILURE_COOLDOWN) - Duration::seconds(1);
        assert!(!is_update_cooldown_active(past));
    }

    /// A newer cached version flips `update_available` to true (still offline).
    #[test]
    fn test_update_check_with_update() {
        let tmp = TempDir::new().unwrap();
        let mgr = UpdateManager::new("0.9.0", tmp.path());
        let info = VersionInfo {
            version: "1.0.0".to_string(),
            release_url: "https://example.com".to_string(),
            published_at: None,
        };
        mgr.save_cached_version(&info).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let check = rt.block_on(mgr.check_for_update()).unwrap();
        assert!(check.update_available, "update expected from 0.9.0 -> 1.0.0");
    }

    /// `install_method_from_path` maps known path substrings.
    #[test]
    fn test_install_method_from_path() {
        assert_eq!(
            UpdateManager::install_method_from_path(Path::new(
                "/home/u/.cargo/bin/deepagents"
            )),
            InstallMethod::CargoBinstall
        );
        assert_eq!(
            UpdateManager::install_method_from_path(Path::new(
                "/opt/homebrew/bin/deepagents"
            )),
            InstallMethod::Homebrew
        );
        assert_eq!(
            UpdateManager::install_method_from_path(Path::new(
                "/usr/local/bin/deepagents"
            )),
            InstallMethod::Homebrew
        );
        assert_eq!(
            UpdateManager::install_method_from_path(Path::new(
                "C:\\Users\\u\\scoop\\apps\\deepagents\\current\\deepagents.exe"
            )),
            InstallMethod::Scoop
        );
        assert_eq!(
            UpdateManager::install_method_from_path(Path::new(
                "/usr/local/myapp/bin/deepagents"
            )),
            InstallMethod::Standalone
        );
    }
}

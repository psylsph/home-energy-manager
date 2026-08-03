//! "New version available" detection.
//!
//! The app has no built-in auto-updater, but it can still tell the user when
//! a newer release is out. This module mirrors the weather / Octopus pattern:
//! a background loop periodically fetches the latest release tag from the
//! GitHub Releases API and caches it in [`AppState::update`]; the
//! [`get_latest_version`] HTTP handler only ever *reads* that cache — it never
//! makes a network call on the request path, so the endpoint stays fast and
//! hermetic to test.
//!
//! The frontend compares `current_version` (the compile-time
//! `CARGO_PKG_VERSION`) against the cached `latest_version` and shows a
//! dismissible banner when the cached release is newer.
//!
//! Privacy: the only thing sent to GitHub is the user's IP (unauthenticated
//! `GET` to `api.github.com`). The whole feature is gated behind a Settings
//! toggle (`check_for_updates`, default on) — when it's off the loop never
//! fetches and the handler reports `disabled: true`.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::inverter::poll::AppState;
use crate::settings::Settings;

/// GitHub Releases endpoint. Unauthenticated calls are rate-limited to
/// 60/hour/IP; the 6-hour loop cadence keeps us well under that.
const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/psylsph/home-energy-manager/releases/latest";

/// How long a cached release is considered fresh. Short enough that a user
/// sees a same-day release within hours; long enough to stay far under
/// GitHub's unauthenticated rate limit (60 req/hour/IP).
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Initial delay before the first fetch. Keeps startup snappy — the release
/// check is low priority and the inverter connection / history should take
/// precedence for the first few seconds.
const INITIAL_DELAY: Duration = Duration::from_secs(30);

/// Per-request timeout for the GitHub HTTP call.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// A user-agent is recommended by the GitHub API and keeps us off generic
/// blocking lists. Includes the current version so the release check itself
/// shows up as identifiable traffic.
const USER_AGENT: &str = concat!(
    "home-energy-manager/",
    env!("CARGO_PKG_VERSION")
);

/// Cached result of the last successful release fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRelease {
    /// Latest release version with any leading `v` stripped (e.g. `"0.70.3"`).
    pub version: String,
    /// Browser-friendly URL of the release (opened by the banner's link).
    pub release_url: String,
}

/// Mutable cache shared between the background loop and the HTTP handler.
///
/// Stored in [`AppState::update`] behind a `tokio::sync::Mutex`. All updates
/// happen in [`run_update_loop`]; the handler only clones the current value,
/// so the lock is never held across a network call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateState {
    /// The most recent release we successfully fetched, or `None` if the
    /// loop has not yet completed a fetch (or the last one failed and no
    /// prior value exists).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<CachedRelease>,
    /// When the cache was last refreshed, regardless of success/failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<DateTime<Utc>>,
    /// Human-readable failure reason from the most recent fetch attempt.
    /// Surfaced (best-effort) in the API response so a curious user can see
    /// why no version is showing — e.g. GitHub unreachable, rate limited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// True while a fetch is in flight. Read by the handler so the frontend
    /// can show "checking…" rather than a bare empty state on first load.
    pub checking: bool,
}

/// Strip a leading `v`/`V` from a version tag (`"v0.70.2"` → `"0.70.2"`).
/// Trims surrounding whitespace first so a stray space doesn't break parsing.
pub fn strip_v(tag: &str) -> &str {
    let trimmed = tag.trim();
    trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed)
}

/// Parse the leading `major.minor.patch` triple out of a version string.
///
/// Anything after the third numeric component (prerelease suffixes like
/// `-rc.1`, build metadata, stray text) is ignored. Returns `None` if the
/// string doesn't begin with at least `N.N.N`.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let cleaned = strip_v(s);
    let mut parts = cleaned.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    // The patch component may carry a prerelease suffix (`3-rc.1`); take the
    // leading run of digits.
    let patch_raw = parts.next()?;
    let patch_digits: String = patch_raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    if patch_digits.is_empty() {
        return None;
    }
    let patch = patch_digits.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// True when `latest` is a strictly newer release than `current`.
///
/// Returns `false` if either string fails to parse (defensive: never claim an
/// update is available on a parse error — a malformed GitHub response should
/// not produce a spurious banner).
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Shape of the subset of the GitHub Releases payload we care about.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// Fetch the latest release from GitHub (blocking — run via `spawn_blocking`).
fn http_fetch_latest_release() -> Result<CachedRelease, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .max_idle_connections(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(GITHUB_RELEASES_URL)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GitHub release request failed: {e}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("failed to read GitHub response: {e}"))?;
    let release: GithubRelease = serde_json::from_str(&body)
        .map_err(|e| format!("invalid GitHub release response: {e}"))?;
    Ok(CachedRelease {
        version: strip_v(&release.tag_name).to_string(),
        release_url: release.html_url,
    })
}

/// Refresh the cached release state with a single fetch. Sets `checking`
/// while the network call is in flight so the handler can report it.
async fn refresh(state: &std::sync::Arc<AppState>) {
    {
        let mut guard = state.update.lock().await;
        guard.checking = true;
    }
    let result = tokio::task::spawn_blocking(http_fetch_latest_release)
        .await
        .map_err(|e| format!("release check task failed: {e}"));
    let mut guard = state.update.lock().await;
    guard.checking = false;
    guard.last_checked_at = Some(Utc::now());
    match result {
        Ok(Ok(release)) => {
            tracing::info!(
                "Latest release: {} (current {})",
                release.version,
                env!("CARGO_PKG_VERSION")
            );
            guard.latest = Some(release);
            guard.last_error = None;
        }
        Ok(Err(error)) | Err(error) => {
            tracing::warn!("Release check failed: {error}");
            // Keep the previous `latest` if we have one; only record the error.
            guard.last_error = Some(error);
        }
    }
}

/// Background loop: periodically fetch the latest release when the user has
/// the feature enabled. Spawned (like the weather / Octopus loops) from the
/// Tauri and headless startup paths in [`crate::lib`]. Not spawned in tests,
/// which keeps the endpoint hermetic.
pub async fn run_update_loop(state: std::sync::Arc<AppState>) {
    tracing::info!("Update checker loop starting");
    // Stagger the first fetch away from startup so inverter connection and
    // history get a clean run at the network first.
    tokio::time::sleep(INITIAL_DELAY).await;

    let mut tick = tokio::time::interval(CHECK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The interval yields its first tick immediately; since we already waited
    // INITIAL_DELAY above, eat that first tick and start the real cadence.
    tick.tick().await;
    loop {
        tick.tick().await;
        if Settings::load().check_for_updates {
            refresh(&state).await;
        }
    }
}

/// `GET /api/latest-version` — current vs latest version, read straight from
/// the cache. Never fetches on the request path (the background loop owns the
/// network); returns `disabled: true` when the user has opted out, and
/// `update_available: false` with no `latest_version` while the cache is still
/// empty (first ~30s after startup, or while GitHub is unreachable).
pub async fn get_latest_version(State(state): State<std::sync::Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let check_enabled = Settings::load().check_for_updates;
    let current = env!("CARGO_PKG_VERSION");

    if !check_enabled {
        return (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "disabled": true,
                "current_version": current,
            })),
        );
    }

    let cached = state.update.lock().await.clone();
    let (latest_version, release_url, update_available) = match &cached.latest {
        Some(release) => (
            Some(release.version.clone()),
            Some(release.release_url.clone()),
            is_newer(&release.version, current),
        ),
        None => (None, None, false),
    };

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "current_version": current,
            "latest_version": latest_version,
            "release_url": release_url,
            "update_available": update_available,
            "checking": cached.checking,
            "last_checked_at": cached.last_checked_at,
            "last_error": cached.last_error,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_v_handles_prefixes() {
        assert_eq!(strip_v("v0.70.2"), "0.70.2");
        assert_eq!(strip_v("V0.70.2"), "0.70.2");
        assert_eq!(strip_v("0.70.2"), "0.70.2");
        assert_eq!(strip_v("  v0.70.2  "), "0.70.2");
        assert_eq!(strip_v(""), "");
    }

    #[test]
    fn parse_version_basic() {
        assert_eq!(parse_version("0.70.2"), Some((0, 70, 2)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("V2.0.0"), Some((2, 0, 0)));
    }

    #[test]
    fn parse_version_ignores_prerelease_suffix() {
        // GitHub sometimes tags prereleases as `0.71.0-rc.1`. We treat it as
        // 0.71.0 for comparison — close enough for a "newer release" banner.
        assert_eq!(parse_version("v0.71.0-rc.1"), Some((0, 71, 0)));
        assert_eq!(parse_version("1.2.3+build.5"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_version_rejects_garbage() {
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.x"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn is_newer_comparisons() {
        assert!(is_newer("0.70.3", "0.70.2"));
        assert!(is_newer("0.71.0", "0.70.99"));
        assert!(is_newer("1.0.0", "0.99.99"));
        // Equal → not newer.
        assert!(!is_newer("0.70.2", "0.70.2"));
        // Older latest → not newer (e.g. running a pre-release/build ahead).
        assert!(!is_newer("0.70.1", "0.70.2"));
    }

    #[test]
    fn is_newer_safe_on_parse_failure() {
        // A malformed "latest" must never trigger a spurious banner.
        assert!(!is_newer("garbage", "0.70.2"));
        assert!(!is_newer("0.70.3", "garbage"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn parse_github_release_payload() {
        // Minimal subset of the real api.github.com response shape.
        let body = r#"{
            "tag_name": "v0.70.3",
            "html_url": "https://github.com/psylsph/home-energy-manager/releases/tag/v0.70.3"
        }"#;
        let release: GithubRelease = serde_json::from_str(body).unwrap();
        assert_eq!(release.tag_name, "v0.70.3");
        let cached = CachedRelease {
            version: strip_v(&release.tag_name).to_string(),
            release_url: release.html_url,
        };
        assert_eq!(cached.version, "0.70.3");
        assert!(cached.release_url.starts_with("https://"));
    }
}

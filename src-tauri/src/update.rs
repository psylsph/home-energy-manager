//! "New version available" detection.
//!
//! The app has no built-in auto-updater, but it can still tell the user when
//! a newer release is out. This module mirrors the weather / Octopus pattern:
//! a background loop periodically fetches the latest release tag from the
//! GitHub Releases API and caches it in [`AppState::update`]. The
//! [`get_latest_version`] HTTP handler reads that cache for the response, but
//! *also* triggers a background refresh when the cache is stale — so poking
//! the endpoint forces a fresh check without waiting for the 6-hour loop.
//!
//! The frontend compares `current_version` (the compile-time
//! `CARGO_PKG_VERSION`) against the cached `latest_version` and shows a
//! dismissible banner when the cached release is newer.
//!
//! Privacy: the only thing sent to GitHub is the user's IP (unauthenticated
//! `GET` to `api.github.com`). The whole feature is gated behind a Settings
//! toggle (`check_for_updates`, default on) — when it's off neither the loop
//! nor the on-demand refresh ever fetches, and the handler reports
//! `disabled: true`.

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

/// Minimum time between on-demand refreshes triggered by the HTTP endpoint.
/// Prevents a user (or a script) from exhausting GitHub's 60/hour/IP
/// unauthenticated rate limit by repeatedly poking the endpoint. At most
/// one on-demand fetch per minute keeps us well under the ceiling even
/// before counting the 6-hour background loop.
const ON_DEMAND_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Initial delay before the first fetch. Keeps startup snappy — the release
/// check is low priority and the inverter connection / history should take
/// precedence for the first few seconds.
const INITIAL_DELAY: Duration = Duration::from_secs(30);

/// Per-request timeout for the GitHub HTTP call.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// A user-agent is recommended by the GitHub API and keeps us off generic
/// blocking lists. Includes the current version so the release check itself
/// shows up as identifiable traffic.
const USER_AGENT: &str = concat!("home-energy-manager/", env!("CARGO_PKG_VERSION"));

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
    /// True once [`run_update_loop`] has started. The HTTP handler uses this
    /// to gate on-demand refreshes so integration tests (which don't spawn
    /// the loop) never trigger a network call.
    #[serde(skip)]
    pub loop_registered: bool,
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
    let patch_digits: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
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
    let release: GithubRelease =
        serde_json::from_str(&body).map_err(|e| format!("invalid GitHub release response: {e}"))?;
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
    // Mark the loop as registered before the initial delay so the HTTP
    // handler knows it's safe to trigger on-demand refreshes (tests don't
    // spawn the loop, so they stay hermetic).
    state.update.lock().await.loop_registered = true;
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

/// Decide whether poking `GET /api/latest-version` should trigger a
/// background refresh. Returns true when the background loop has started
/// (so tests stay hermetic), no fetch is already in flight, and the cache
/// is either empty or older than [`ON_DEMAND_MIN_INTERVAL`].
fn should_trigger_on_demand_refresh(cached: &UpdateState) -> bool {
    if !cached.loop_registered || cached.checking {
        return false;
    }
    match cached.last_checked_at {
        None => true, // never checked — cold cache
        Some(last) => {
            let elapsed = Utc::now().signed_duration_since(last);
            elapsed.num_seconds() >= ON_DEMAND_MIN_INTERVAL.as_secs() as i64
        }
    }
}

/// Atomically decide whether an on-demand refresh should start AND claim it.
///
/// The decision and the `checking = true` claim happen inside a single lock
/// scope, so of N concurrent pokes of `GET /api/latest-version` exactly one
/// caller wins and spawns a fetch — the losers observe `checking` (or the
/// freshly-updated `last_checked_at`) and back off. Without the combined
/// lock scope, every concurrent caller would clone the same stale cache,
/// each decide "refresh needed", and each spawn a fetch (GitHub's
/// unauthenticated limit is 60/hour/IP).
///
/// On success also stamps `last_checked_at` immediately, so a delayed spawn
/// can't slip past the rate-limit guard once `checking` is cleared.
async fn try_begin_on_demand_refresh(state: &std::sync::Arc<AppState>) -> bool {
    let mut guard = state.update.lock().await;
    if !should_trigger_on_demand_refresh(&guard) {
        return false;
    }
    guard.checking = true;
    guard.last_checked_at = Some(Utc::now());
    true
}

/// `GET /api/latest-version` — current vs latest version, read from the
/// cache. Also triggers a background refresh when the cache is stale, so
/// poking the endpoint forces a fresh check without waiting for the 6-hour
/// loop. The response itself returns the current cache immediately (the
/// refresh runs in the background and the frontend's retry picks it up).
/// Returns `disabled: true` when the user has opted out, and
/// `update_available: false` with no `latest_version` while the cache is
/// still empty (first ~30s after startup, or while GitHub is unreachable).
pub async fn get_latest_version(
    State(state): State<std::sync::Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
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

    // Trigger a background refresh when the cache is stale. The claim
    // (`checking = true` + `last_checked_at` stamp) happens atomically inside
    // `try_begin_on_demand_refresh`, so N concurrent pokes of this endpoint
    // result in exactly one fetch — the rate-limit guards actually bound
    // concurrent traffic. Non-blocking: the response below returns the
    // current cache immediately; the frontend's retry (or the next endpoint
    // hit) picks up the refreshed data once the fetch completes.
    if try_begin_on_demand_refresh(&state).await {
        let state_clone = state.clone();
        tokio::spawn(async move {
            refresh(&state_clone).await;
        });
    }

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

    #[tokio::test]
    async fn concurrent_pokes_trigger_exactly_one_on_demand_refresh() {
        // Regression test for the on-demand refresh race: the decision and
        // the `checking = true` claim must happen inside one lock scope, so
        // a burst of N simultaneous pokes of GET /api/latest-version results
        // in exactly one background fetch instead of N (GitHub's
        // unauthenticated limit is 60/hour/IP).
        let state = std::sync::Arc::new(AppState::new());
        state.update.lock().await.loop_registered = true;

        let mut handles = Vec::new();
        for _ in 0..16 {
            let state = state.clone();
            handles.push(tokio::spawn(async move {
                try_begin_on_demand_refresh(&state).await
            }));
        }
        let mut winners = 0usize;
        for handle in handles {
            winners += handle.await.unwrap() as usize;
        }
        assert_eq!(winners, 1, "exactly one concurrent poke must win the fetch");
        // The winner left `checking` set so losers (and later pokes) back off.
        assert!(state.update.lock().await.checking);
    }

    #[tokio::test]
    async fn sequential_pokes_respect_min_interval_then_retrigger() {
        // After a fetch completes (checking cleared, last_checked_at = now),
        // further pokes within ON_DEMAND_MIN_INTERVAL must not re-fetch —
        // but once the interval expires, a poke triggers again.
        let state = std::sync::Arc::new(AppState::new());
        state.update.lock().await.loop_registered = true;

        // First poke on a cold cache wins and claims `checking`.
        assert!(try_begin_on_demand_refresh(&state).await);
        // Simulate the fetch completing.
        {
            let mut guard = state.update.lock().await;
            guard.checking = false;
            guard.last_checked_at = Some(Utc::now());
        }
        // Pokes within the interval are rate-limited out.
        assert!(!try_begin_on_demand_refresh(&state).await);
        assert!(!try_begin_on_demand_refresh(&state).await);
        // Once the cache is older than the interval, a poke triggers again.
        {
            let mut guard = state.update.lock().await;
            guard.last_checked_at = Some(
                Utc::now() - chrono::Duration::seconds(ON_DEMAND_MIN_INTERVAL.as_secs() as i64 + 1),
            );
        }
        assert!(try_begin_on_demand_refresh(&state).await);
    }

    // -----------------------------------------------------------------------
    // should_trigger_on_demand_refresh: the decision that gates whether poking
    // GET /api/latest-version spawns a background GitHub fetch. Pure function —
    // no network, instant.
    // -----------------------------------------------------------------------

    #[test]
    fn on_demand_refresh_triggers_on_cold_cache_when_registered() {
        // The background loop has started but hasn't fetched yet. This is the
        // cold-start case: the user opens the app, the frontend pokes the
        // endpoint, and we should trigger an immediate fetch.
        let state = UpdateState {
            loop_registered: true,
            ..Default::default()
        };
        assert!(should_trigger_on_demand_refresh(&state));
    }

    #[test]
    fn on_demand_refresh_skipped_when_loop_not_registered() {
        // Tests don't spawn the loop, so loop_registered stays false and the
        // handler never triggers a network call — this is the hermeticity
        // guard.
        let state = UpdateState::default();
        assert!(!should_trigger_on_demand_refresh(&state));
    }

    #[test]
    fn on_demand_refresh_skipped_while_checking() {
        // A fetch is already in flight — don't start a duplicate.
        let state = UpdateState {
            loop_registered: true,
            checking: true,
            ..Default::default()
        };
        assert!(!should_trigger_on_demand_refresh(&state));
    }

    #[test]
    fn on_demand_refresh_skipped_within_min_interval() {
        // Cache was checked moments ago — don't re-fetch so soon (rate-limit
        // protection).
        let state = UpdateState {
            loop_registered: true,
            last_checked_at: Some(Utc::now()),
            ..Default::default()
        };
        assert!(!should_trigger_on_demand_refresh(&state));
    }

    #[test]
    fn on_demand_refresh_triggers_after_interval_expires() {
        // Cache is older than ON_DEMAND_MIN_INTERVAL — safe to re-fetch.
        let state = UpdateState {
            loop_registered: true,
            last_checked_at: Some(
                Utc::now() - chrono::Duration::seconds(ON_DEMAND_MIN_INTERVAL.as_secs() as i64 + 1),
            ),
            ..Default::default()
        };
        assert!(should_trigger_on_demand_refresh(&state));
    }

    // ================================================================
    // get_latest_version handler — response shape per cache/opt-in state
    // ================================================================

    #[tokio::test]
    async fn get_latest_version_reports_disabled_when_opted_out() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let mut settings = Settings::load();
            settings.check_for_updates = false;
            settings.save().unwrap();

            let state = std::sync::Arc::new(AppState::new());
            let (status, body) = get_latest_version(State(state)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], true);
            assert_eq!(body["disabled"], true);
            assert_eq!(body["current_version"], env!("CARGO_PKG_VERSION"));
            // No version fields are emitted when the feature is off.
            assert!(body.get("latest_version").is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn get_latest_version_serves_empty_cache_then_cached_release() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let mut settings = Settings::load();
            settings.check_for_updates = true;
            settings.save().unwrap();
            let state = std::sync::Arc::new(AppState::new());

            // Empty cache (loop not registered → no on-demand refresh fires):
            // no latest_version, no banner.
            let (status, body) = get_latest_version(State(state.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["latest_version"], Value::Null);
            assert_eq!(body["update_available"], false);
            assert_eq!(body["current_version"], env!("CARGO_PKG_VERSION"));

            // Populate the cache with a newer release → banner fires.
            {
                let mut u = state.update.lock().await.clone();
                u.latest = Some(CachedRelease {
                    version: "99.99.99".to_string(),
                    release_url: "https://example/release".to_string(),
                });
                *state.update.lock().await = u;
            }
            let (_, body) = get_latest_version(State(state.clone())).await;
            assert_eq!(body["latest_version"], "99.99.99");
            assert_eq!(body["release_url"], "https://example/release");
            assert_eq!(body["update_available"], true);

            // A cached release older than the running version → no banner.
            {
                let mut u = state.update.lock().await.clone();
                u.latest = Some(CachedRelease {
                    version: "0.0.1".to_string(),
                    release_url: "https://example/old".to_string(),
                });
                *state.update.lock().await = u;
            }
            let (_, body) = get_latest_version(State(state)).await;
            assert_eq!(body["latest_version"], "0.0.1");
            assert_eq!(body["update_available"], false);
        })
        .await;
    }
}

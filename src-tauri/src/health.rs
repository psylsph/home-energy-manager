//! HEM's own health, for external monitors (issue #503).
//!
//! `GET /api/health` (see [`crate::server::health`]) answers one overall
//! `healthy` / `degraded` / `unhealthy` with an entry per check. Everything
//! here is what HEM already knows: no inverter reads, no network calls and
//! no database queries, so a monitor can ask every minute.
//!
//! Only the core checks (the inverter, the poll loop and history) can make
//! the answer `unhealthy`; the rest only degrade it. A feature that isn't set
//! up is `disabled` and never counts. Each check's `detail` is a fixed code,
//! never free text, because the endpoint needs no key.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::inverter::poll::ConnectionState;

/// How long after start a missing first reading or poll beat reads as
/// `starting` rather than a failure.
const STARTUP_GRACE_S: i64 = 5 * 60;

/// Budget for a beat taken before work that can legitimately take a while:
/// a poll cycle (reads with retries, a queue of writes each retried while
/// the dongle is busy), a connect and its warm-up, or a LAN scan.
pub const POLL_BUSY_BUDGET: Duration = Duration::from_secs(15 * 60);

/// Budget for a beat taken inside one of the loop's once-a-second waits.
pub const POLL_IDLE_BUDGET: Duration = Duration::from_secs(2 * 60);

/// Free space under which the disk check warns. A backup short of room
/// reports itself through the backup check.
const DISK_WARN_BYTES: u64 = 500 * 1024 * 1024;

/// Free space under which the disk check fails.
const DISK_FAIL_BYTES: u64 = 100 * 1024 * 1024;

/// Weather is fetched every 15 minutes; four missed fetches is a problem.
const WEATHER_STALE_S: i64 = 60 * 60;

/// The solar forecast is fetched every 3 hours: two missed and an hour over.
const SOLAR_FORECAST_STALE_S: i64 = 7 * 60 * 60;

/// Octopus syncs every 6 hours: two missed and an hour over.
const OCTOPUS_STALE_S: i64 = 13 * 60 * 60;

// ---------------------------------------------------------------------------
// Marks the running parts of HEM leave for the health check
// ---------------------------------------------------------------------------

/// Process start, the latest reading and the poll loop's heartbeat, in
/// [`crate::inverter::poll::AppState`].
///
/// Every age judged here is measured on the monotonic clock (milliseconds
/// after `started`), so a wall clock stepping either way, such as a Pi
/// booting with a stale time until NTP corrects it, can neither hide an
/// outage nor invent one. Wall times are kept only to show `last_ok`.
#[derive(Debug)]
pub struct HealthMarks {
    started: Instant,
    /// Milliseconds after `started` by which the poll loop said it would beat
    /// again; 0 before its first beat.
    poll_due_ms: AtomicU64,
    /// Wall-clock seconds of the poll loop's latest beat; 0 before the first.
    poll_beat_at: AtomicI64,
    /// When the poll cycle now running started (ms after `started`, plus one
    /// so 0 can mean none): set at the top of a connected poll cycle, cleared
    /// when the loop goes back to waiting.
    cycle_started_ms: AtomicU64,
    /// The latest published reading's own timestamp, for `last_ok`; 0 before
    /// the first. Kept across reconnects (unlike `latest_snapshot`, which a
    /// disconnect clears), so an outage still shows when HEM last had data.
    reading_at: AtomicI64,
    /// When that reading was published (ms after `started`, plus one).
    reading_ms: AtomicU64,
    /// Per [`Feature`], when the health check first saw it enabled (ms after
    /// `started`), or [`UNOBSERVED`] / [`DISABLED`].
    enabled_since_ms: [AtomicI64; FEATURES],
}

/// The optional features whose first fetch gets a grace from being enabled.
#[derive(Debug, Clone, Copy)]
pub enum Feature {
    Backup = 0,
    Weather = 1,
    SolarForecast = 2,
    Octopus = 3,
}

const FEATURES: usize = 4;
const DISABLED: i64 = -1;

impl Default for HealthMarks {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

/// What the poll loop's heartbeat shows at a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollBeat {
    /// The loop hasn't beaten yet.
    NotYet,
    /// The latest beat (wall seconds) and whether its budget has run out.
    Beat { at: i64, overdue: bool },
}

/// The latest reading as the inverter check sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The reading's own timestamp (unix seconds), for `last_ok`.
    pub at: i64,
    /// How old it counts as, in seconds, on the monotonic clock.
    pub age_s: i64,
}

/// When a fetch or sync last worked: its wall time, shown as `last_ok`, and
/// its age, which is what's judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastOk {
    pub at: i64,
    pub age_s: i64,
}

impl LastOk {
    /// A success this process saw at wall time `at` and monotonic `marked`,
    /// aged on the monotonic clock at `now`, so a clock step can neither
    /// fake nor hide staleness.
    pub fn since(at: i64, marked: Instant, now: Instant) -> Self {
        let age_s =
            i64::try_from(now.saturating_duration_since(marked).as_secs()).unwrap_or(i64::MAX);
        Self { at, age_s }
    }
}

impl HealthMarks {
    pub fn new(started: Instant) -> Self {
        Self {
            started,
            poll_due_ms: AtomicU64::new(0),
            poll_beat_at: AtomicI64::new(0),
            cycle_started_ms: AtomicU64::new(0),
            reading_at: AtomicI64::new(0),
            reading_ms: AtomicU64::new(0),
            enabled_since_ms: std::array::from_fn(|_| AtomicI64::new(DISABLED)),
        }
    }

    /// [`Self::new`], with the features already `on` at start counted as
    /// enabled since then.
    pub fn with_features_on(started: Instant, on: &[(Feature, bool)]) -> Self {
        let marks = Self::new(started);
        for &(feature, enabled) in on {
            if enabled {
                marks.enabled_since_ms[feature as usize].store(0, Ordering::Relaxed);
            }
        }
        marks
    }

    /// Milliseconds after start at `now`, never negative.
    fn ms(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.started).as_millis()).unwrap_or(u64::MAX)
    }

    /// A reading taken at `timestamp` was published now.
    pub fn note_reading(&self, timestamp: i64) {
        self.note_reading_at(Instant::now(), timestamp);
    }

    /// [`Self::note_reading`] at an injected time.
    pub fn note_reading_at(&self, now: Instant, timestamp: i64) {
        self.reading_at.store(timestamp.max(1), Ordering::Relaxed);
        self.reading_ms
            .store(self.ms(now).saturating_add(1), Ordering::Relaxed);
    }

    /// The latest reading at `now`, if there has been one.
    ///
    /// While a connected poll cycle that started after that reading is still
    /// running, and is within [`POLL_BUSY_BUDGET`], its age is counted only
    /// up to the cycle's start: a cycle working through a queue of writes the
    /// dongle keeps refusing can run for minutes before it reads, and that is
    /// the poll loop being busy, not the inverter going quiet. Past the
    /// budget the cycle is stuck, and the age is the real one again.
    pub fn reading(&self, now: Instant) -> Option<Reading> {
        let read_ms = self.reading_ms.load(Ordering::Relaxed).checked_sub(1)?;
        let now_ms = self.ms(now);
        let budget_ms = u64::try_from(POLL_BUSY_BUDGET.as_millis()).unwrap_or(u64::MAX);
        let cycle = self.cycle_started_ms.load(Ordering::Relaxed).checked_sub(1);
        let until_ms = match cycle {
            Some(cycle_ms)
                if cycle_ms >= read_ms && cycle_ms <= now_ms && now_ms - cycle_ms <= budget_ms =>
            {
                cycle_ms
            }
            _ => now_ms,
        };
        let age_s = i64::try_from(until_ms.saturating_sub(read_ms) / 1000).unwrap_or(i64::MAX);
        Some(Reading {
            at: self.reading_at.load(Ordering::Relaxed),
            age_s,
        })
    }

    /// How long `feature` has been enabled at `now`, in seconds (0 while
    /// disabled). A feature on at start (see [`Self::with_features_on`])
    /// counts from start; one switched on later, from the first look that
    /// saw it on, so a monitor set up after the feature still gives it its
    /// grace.
    pub fn enabled_for_s(&self, feature: Feature, enabled: bool, now: Instant) -> i64 {
        let slot = &self.enabled_since_ms[feature as usize];
        if !enabled {
            slot.store(DISABLED, Ordering::Relaxed);
            return 0;
        }
        let now_ms = i64::try_from(self.ms(now)).unwrap_or(i64::MAX);
        let since_ms = match slot.load(Ordering::Relaxed) {
            DISABLED => now_ms,
            since => since,
        };
        slot.store(since_ms, Ordering::Relaxed);
        (now_ms - since_ms).max(0) / 1000
    }

    /// Seconds since start at `now`.
    pub fn uptime_s(&self, now: Instant) -> i64 {
        i64::try_from(now.saturating_duration_since(self.started).as_secs()).unwrap_or(i64::MAX)
    }

    /// The poll loop is alive now and will beat again within `budget`. It is
    /// waiting, or connecting, not in a connected poll cycle.
    pub fn poll_beat(&self, budget: Duration) {
        self.poll_beat_at_time(Instant::now(), chrono::Utc::now().timestamp(), budget);
    }

    /// [`Self::poll_beat`] at injected times.
    pub fn poll_beat_at_time(&self, now: Instant, now_wall: i64, budget: Duration) {
        self.beat(now, now_wall, budget);
        self.cycle_started_ms.store(0, Ordering::Relaxed);
    }

    /// A connected poll cycle (reads, then any queued writes) starts now.
    pub fn poll_cycle_start(&self) {
        self.poll_cycle_start_at(Instant::now(), chrono::Utc::now().timestamp());
    }

    /// [`Self::poll_cycle_start`] at injected times.
    pub fn poll_cycle_start_at(&self, now: Instant, now_wall: i64) {
        self.beat(now, now_wall, POLL_BUSY_BUDGET);
        self.cycle_started_ms
            .store(self.ms(now).saturating_add(1), Ordering::Relaxed);
    }

    fn beat(&self, now: Instant, now_wall: i64, budget: Duration) {
        let due = now.saturating_duration_since(self.started) + budget;
        let due_ms = u64::try_from(due.as_millis()).unwrap_or(u64::MAX).max(1);
        self.poll_due_ms.store(due_ms, Ordering::Relaxed);
        self.poll_beat_at.store(now_wall, Ordering::Relaxed);
    }

    /// The heartbeat as seen at `now`.
    pub fn poll_state(&self, now: Instant) -> PollBeat {
        let due_ms = self.poll_due_ms.load(Ordering::Relaxed);
        if due_ms == 0 {
            return PollBeat::NotYet;
        }
        PollBeat::Beat {
            at: self.poll_beat_at.load(Ordering::Relaxed),
            overdue: self.ms(now) > due_ms,
        }
    }
}

/// When history writes last succeeded and failed, and how long minute rows
/// have been waiting to be written. Kept on [`crate::history::HistoryDb`] so
/// the health check never takes the database lock.
///
/// Whether the latest write failed is a flag, not a comparison of the two
/// times, so a clock stepping back can't leave a recovered writer failing;
/// the wait is on the monotonic clock, so a clock step can't fake or hide it.
#[derive(Debug, Default)]
pub struct WriteMarks {
    ok_at: AtomicI64,
    err_at: AtomicI64,
    failing: AtomicBool,
    wait: parking_lot::Mutex<Wait>,
}

/// The oldest unwritten rows' ready time, and the latest rows' ready time.
#[derive(Debug, Default, Clone, Copy)]
struct Wait {
    since: Option<Instant>,
    latest: Option<Instant>,
}

/// A copy of [`WriteMarks`] at one moment; 0 means never.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryWrites {
    pub ok_at: i64,
    pub err_at: i64,
    /// How long ready rows have waited to be written, if any are waiting.
    pub waited_s: Option<i64>,
    /// The latest write failed.
    pub failing: bool,
}

impl WriteMarks {
    /// Rows became ready at `now`; keeps the earliest unwritten time.
    pub fn rows_ready(&self, now: Instant) {
        let mut wait = self.wait.lock();
        wait.since.get_or_insert(now);
        wait.latest = Some(now);
    }

    /// A write succeeded at `now` (unix seconds). Plain stores: a time stamped
    /// while the clock was ahead must not hide staleness after it is corrected.
    /// `more_queued`: rows queued since this write's batch was taken are still
    /// waiting; they count from the latest ready time.
    pub fn write_ok(&self, now: i64, more_queued: bool) {
        self.ok_at.store(now, Ordering::Relaxed);
        self.failing.store(false, Ordering::Relaxed);
        let mut wait = self.wait.lock();
        *wait = if more_queued {
            Wait {
                since: wait.latest,
                latest: wait.latest,
            }
        } else {
            Wait::default()
        };
    }

    /// A write failed at `now` (unix seconds). Its rows are dropped, but their
    /// wait is kept until a write works.
    pub fn write_failed(&self, now: i64) {
        self.err_at.store(now, Ordering::Relaxed);
        self.failing.store(true, Ordering::Relaxed);
    }

    /// While writes are failing the wait runs only up to the latest ready
    /// rows: if the dongle drops after a failed write, no rows come and
    /// nothing waits, so the outage isn't blamed on the writer. Writes that
    /// keep failing while rows keep coming still run it out.
    pub fn snapshot(&self, now: Instant) -> HistoryWrites {
        let failing = self.failing.load(Ordering::Relaxed);
        let wait = *self.wait.lock();
        let waited_s = wait.since.map(|since| {
            let until = if failing {
                wait.latest.unwrap_or(since)
            } else {
                now
            };
            i64::try_from(until.saturating_duration_since(since).as_secs()).unwrap_or(i64::MAX)
        });
        HistoryWrites {
            ok_at: self.ok_at.load(Ordering::Relaxed),
            err_at: self.err_at.load(Ordering::Relaxed),
            waited_s,
            failing,
        }
    }
}

/// When one notification channel last delivered and last failed (unix
/// seconds, 0 = never), and whether its latest send failed. Process-wide
/// statics, one per channel: the senders have no `AppState`.
///
/// The marks belong to one set of credentials (a fingerprint of them). A
/// send with other credentials, one still in flight when the settings were
/// changed, is ignored, so it can't re-flag a channel that was just fixed.
#[derive(Debug, Default)]
pub struct DeliveryMarks {
    inner: parking_lot::Mutex<Delivery>,
}

#[derive(Debug, Default)]
struct Delivery {
    /// The credentials these marks are for; `None` until the first send or
    /// reset, when whichever comes first is adopted.
    key: Option<u64>,
    marks: NotifyMarks,
}

/// A fingerprint of a set of credentials (a notification channel's, or
/// Octopus's), so marks can tell which ones a result came from.
pub fn credentials_key(credentials: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    credentials.hash(&mut hasher);
    hasher.finish()
}

impl DeliveryMarks {
    pub const fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(Delivery {
                key: None,
                marks: NotifyMarks {
                    delivered_at: 0,
                    failed_at: 0,
                    failing: false,
                },
            }),
        }
    }

    /// Forget past sends: the channel's credentials are now `key`.
    pub fn reset(&self, key: u64) {
        *self.inner.lock() = Delivery {
            key: Some(key),
            marks: NotifyMarks::default(),
        };
    }

    /// Record a send made with credentials `key` at `now`.
    pub fn record(&self, key: u64, delivered: bool, now: i64) {
        let mut inner = self.inner.lock();
        if *inner.key.get_or_insert(key) != key {
            return;
        }
        let marks = &mut inner.marks;
        if delivered {
            marks.delivered_at = now;
        } else {
            marks.failed_at = now;
        }
        marks.failing = !delivered;
    }

    pub fn snapshot(&self) -> NotifyMarks {
        self.inner.lock().marks
    }
}

/// A copy of [`DeliveryMarks`] at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotifyMarks {
    pub delivered_at: i64,
    pub failed_at: i64,
    /// The latest send failed.
    pub failing: bool,
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Starting,
    Disabled,
}

/// One check. `detail` is a fixed snake_case code, never free text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub status: CheckStatus,
    pub core: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'static str>,
    /// Unix seconds this part last worked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<i64>,
    /// Seconds since `last_ok`, never negative.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_s: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_mb: Option<u64>,
}

impl Check {
    fn new(core: bool, status: CheckStatus, detail: Option<&'static str>) -> Self {
        Self {
            status,
            core,
            detail,
            last_ok: None,
            age_s: None,
            free_mb: None,
        }
    }

    /// `last_ok` with an age judged elsewhere (see [`LastOk`]).
    fn with_last(mut self, last: Option<LastOk>) -> Self {
        if let Some(last) = last {
            self.last_ok = Some(last.at);
            self.age_s = Some(last.age_s.max(0));
        }
        self
    }

    fn last_ok(mut self, now: i64, at: Option<i64>) -> Self {
        if let Some(at) = at.filter(|&at| at > 0) {
            self.last_ok = Some(at);
            self.age_s = Some((now - at).max(0));
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checks {
    pub inverter: Check,
    pub poll_loop: Check,
    pub history: Check,
    pub backup: Check,
    pub disk: Check,
    pub notifications: Check,
    pub weather: Check,
    pub solar_forecast: Check,
    pub octopus: Check,
}

impl Checks {
    fn all(&self) -> [&Check; 9] {
        [
            &self.inverter,
            &self.poll_loop,
            &self.history,
            &self.backup,
            &self.disk,
            &self.notifications,
            &self.weather,
            &self.solar_forecast,
            &self.octopus,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthReport {
    pub status: Overall,
    pub version: &'static str,
    pub uptime_s: i64,
    pub now: i64,
    pub checks: Checks,
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct BackupInput {
    pub enabled: bool,
    /// Seconds since backups were seen enabled.
    pub enabled_for_s: i64,
    pub interval_hours: u32,
    pub last_success_at: Option<i64>,
    /// Whether the latest attempt (scheduled or manual) worked, if any.
    pub last_attempt_ok: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DiskInput {
    /// `None` when free space couldn't be read.
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct NotifyInput {
    /// Alerts are on and at least one channel is set up.
    pub enabled: bool,
    /// One per configured channel.
    pub channels: Vec<NotifyMarks>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FetchInput {
    pub enabled: bool,
    /// Seconds since the feature was seen enabled.
    pub enabled_for_s: i64,
    /// The latest success in this process.
    pub last_ok: Option<LastOk>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OctopusInput {
    pub configured: bool,
    /// Seconds since it was seen configured.
    pub enabled_for_s: i64,
    /// The latest sync that worked, in this process.
    pub last_sync: Option<LastOk>,
    /// The latest sync failed with the credentials saved now.
    pub failed: bool,
}

/// Everything the report is built from, copied out of the live state.
#[derive(Debug, Clone)]
pub struct HealthInputs {
    /// Unix seconds.
    pub now: i64,
    pub uptime_s: i64,
    pub poll_interval_secs: u64,
    pub host_configured: bool,
    pub connection: ConnectionState,
    /// The latest published reading, kept across reconnects.
    pub reading: Option<Reading>,
    pub poll: PollBeat,
    /// `None` when the history database isn't open.
    pub history: Option<HistoryWrites>,
    pub backup: BackupInput,
    pub disk: DiskInput,
    pub notifications: NotifyInput,
    pub weather: FetchInput,
    pub solar_forecast: FetchInput,
    pub octopus: OctopusInput,
}

// ---------------------------------------------------------------------------
// Judging
// ---------------------------------------------------------------------------

/// Judge every check. Served as 503 when [`Overall::Unhealthy`].
pub fn build_health(inputs: &HealthInputs) -> HealthReport {
    let checks = Checks {
        inverter: inverter_check(inputs),
        poll_loop: poll_check(inputs),
        history: history_check(inputs),
        backup: backup_check(inputs),
        disk: disk_check(inputs.disk),
        notifications: notify_check(inputs.now, &inputs.notifications),
        weather: fetch_check(inputs, inputs.weather, WEATHER_STALE_S),
        solar_forecast: fetch_check(inputs, inputs.solar_forecast, SOLAR_FORECAST_STALE_S),
        octopus: octopus_check(inputs),
    };
    let status = overall(&checks);
    HealthReport {
        status,
        version: env!("CARGO_PKG_VERSION"),
        uptime_s: inputs.uptime_s.max(0),
        now: inputs.now,
        checks,
    }
}

fn overall(checks: &Checks) -> Overall {
    let all = checks.all();
    if all.iter().any(|c| c.core && c.status == CheckStatus::Fail) {
        return Overall::Unhealthy;
    }
    if all.iter().any(|c| {
        matches!(
            c.status,
            CheckStatus::Warn | CheckStatus::Fail | CheckStatus::Starting
        )
    }) {
        return Overall::Degraded;
    }
    Overall::Healthy
}

fn starting(inputs: &HealthInputs) -> bool {
    inputs.uptime_s < STARTUP_GRACE_S
}

/// How old the latest reading may be: three polls, and never under 3 min.
fn reading_max_age_s(inputs: &HealthInputs) -> i64 {
    let interval = i64::try_from(inputs.poll_interval_secs).unwrap_or(i64::MAX / 4);
    interval.saturating_mul(3).max(180)
}

/// Judged by how old the latest reading is, whatever the connection state:
/// a dongle that drops and reconnects within three polls never fails, and
/// one that stays connected without new data does.
fn inverter_check(inputs: &HealthInputs) -> Check {
    use CheckStatus::{Fail, Ok, Starting};
    if !inputs.host_configured {
        return Check::new(true, Fail, Some("not_configured"));
    }
    let connection = match inputs.connection {
        ConnectionState::Connected => "connected",
        ConnectionState::Reconnecting => "reconnecting",
        ConnectionState::Disconnected => "disconnected",
    };
    let status = match inputs.reading {
        Some(r) if r.age_s <= reading_max_age_s(inputs) => Ok,
        None if starting(inputs) => Starting,
        _ => Fail,
    };
    let detail = match (status, inputs.reading, &inputs.connection) {
        (Fail, None, _) => "no_reading",
        (Fail, Some(_), ConnectionState::Connected) => "stale_reading",
        _ => connection,
    };
    let mut check = Check::new(true, status, Some(detail));
    if let Some(r) = inputs.reading {
        // The age comes from the monotonic clock, not `now - at`.
        check.last_ok = Some(r.at);
        check.age_s = Some(r.age_s);
    }
    check
}

fn poll_check(inputs: &HealthInputs) -> Check {
    match inputs.poll {
        PollBeat::NotYet if starting(inputs) => Check::new(true, CheckStatus::Starting, None),
        PollBeat::NotYet => Check::new(true, CheckStatus::Fail, Some("not_running")),
        PollBeat::Beat { at, overdue } => {
            let check = if overdue {
                Check::new(true, CheckStatus::Fail, Some("stuck"))
            } else {
                Check::new(true, CheckStatus::Ok, None)
            };
            check.last_ok(inputs.now, Some(at))
        }
    }
}

/// How long a ready minute row may wait to be written.
fn history_max_wait_s(inputs: &HealthInputs) -> i64 {
    let interval = i64::try_from(inputs.poll_interval_secs).unwrap_or(i64::MAX / 4);
    interval.saturating_mul(2).saturating_add(120).max(300)
}

fn history_check(inputs: &HealthInputs) -> Check {
    let Some(writes) = inputs.history else {
        return Check::new(true, CheckStatus::Fail, Some("unavailable"));
    };
    let waiting_too_long = writes
        .waited_s
        .is_some_and(|waited| waited > history_max_wait_s(inputs));
    // A failed write only degrades: a single SQLite hiccup heals with the
    // next minute's row. Failed rows keep their wait while new rows keep
    // coming, so writes that stay broken fail as not_written once it runs out.
    let check = if waiting_too_long {
        Check::new(true, CheckStatus::Fail, Some("not_written"))
    } else if writes.failing {
        Check::new(true, CheckStatus::Warn, Some("write_failed"))
    } else if writes.ok_at == 0 {
        Check::new(true, CheckStatus::Ok, Some("idle"))
    } else {
        Check::new(true, CheckStatus::Ok, None)
    };
    check.last_ok(inputs.now, Some(writes.ok_at))
}

fn backup_check(inputs: &HealthInputs) -> Check {
    let b = inputs.backup;
    if !b.enabled {
        return Check::new(false, CheckStatus::Disabled, None);
    }
    let interval_s = i64::from(b.interval_hours.max(1)) * 3600;
    let grace_s = interval_s + 3600;
    let check = if b.last_attempt_ok == Some(false) {
        Check::new(false, CheckStatus::Warn, Some("last_failed"))
    } else {
        // The last success is saved across restarts, so unlike the fetches it
        // can only be aged on the wall clock.
        match b.last_success_at {
            None if b.enabled_for_s <= grace_s => {
                Check::new(false, CheckStatus::Ok, Some("not_yet"))
            }
            None => Check::new(false, CheckStatus::Warn, Some("never")),
            Some(at) if inputs.now - at > 2 * interval_s + 3600 => {
                if just_switched_on(inputs, b.enabled_for_s, inputs.now - at, grace_s) {
                    Check::new(false, CheckStatus::Ok, Some("not_yet"))
                } else {
                    Check::new(false, CheckStatus::Warn, Some("overdue"))
                }
            }
            Some(_) => Check::new(false, CheckStatus::Ok, None),
        }
    };
    check.last_ok(inputs.now, b.last_success_at)
}

/// A feature switched on after start whose last success (`age_s` old) is
/// from before then, and which has been on for no more than `grace_s`: it
/// gets the same time for its first update as one that never ran. One on
/// since start doesn't, so a restart can't hide a backup that's overdue.
fn just_switched_on(inputs: &HealthInputs, enabled_for_s: i64, age_s: i64, grace_s: i64) -> bool {
    enabled_for_s < inputs.uptime_s && age_s > enabled_for_s && enabled_for_s <= grace_s
}

fn disk_check(disk: DiskInput) -> Check {
    let Some(free) = disk.free_bytes else {
        return Check::new(false, CheckStatus::Ok, Some("unavailable"));
    };
    let mut check = if free < DISK_FAIL_BYTES {
        Check::new(false, CheckStatus::Fail, Some("very_low_space"))
    } else if free < DISK_WARN_BYTES {
        Check::new(false, CheckStatus::Warn, Some("low_space"))
    } else {
        Check::new(false, CheckStatus::Ok, None)
    };
    check.free_mb = Some(free / (1024 * 1024));
    check
}

/// Warns when any configured channel's latest send failed, so one dead
/// channel shows even while another still gets through.
fn notify_check(now: i64, n: &NotifyInput) -> Check {
    if !n.enabled {
        return Check::new(false, CheckStatus::Disabled, None);
    }
    let delivered_at = n.channels.iter().map(|c| c.delivered_at).max().unwrap_or(0);
    let check = if n.channels.iter().any(|c| c.failing) {
        Check::new(false, CheckStatus::Warn, Some("last_failed"))
    } else if delivered_at == 0 {
        Check::new(false, CheckStatus::Ok, Some("none_sent"))
    } else {
        Check::new(false, CheckStatus::Ok, None)
    };
    check.last_ok(now, Some(delivered_at))
}

/// A fetch older than `stale_after_s` is stale; one that has never run, or
/// last ran before the feature was switched back on, is fine until the
/// feature has been on that long.
fn fetch_check(inputs: &HealthInputs, fetch: FetchInput, stale_after_s: i64) -> Check {
    if !fetch.enabled {
        return Check::new(false, CheckStatus::Disabled, None);
    }
    let check = match fetch.last_ok {
        None if fetch.enabled_for_s <= stale_after_s => {
            Check::new(false, CheckStatus::Ok, Some("not_yet"))
        }
        None => Check::new(false, CheckStatus::Warn, Some("never")),
        Some(last) if last.age_s > stale_after_s => {
            if just_switched_on(inputs, fetch.enabled_for_s, last.age_s, stale_after_s) {
                Check::new(false, CheckStatus::Ok, Some("not_yet"))
            } else {
                Check::new(false, CheckStatus::Warn, Some("stale"))
            }
        }
        Some(_) => Check::new(false, CheckStatus::Ok, None),
    };
    check.with_last(fetch.last_ok)
}

fn octopus_check(inputs: &HealthInputs) -> Check {
    let o = inputs.octopus;
    if o.configured && o.failed {
        return Check::new(false, CheckStatus::Warn, Some("last_failed")).with_last(o.last_sync);
    }
    let sync = FetchInput {
        enabled: o.configured,
        enabled_for_s: o.enabled_for_s,
        last_ok: o.last_sync,
    };
    fetch_check(inputs, sync, OCTOPUS_STALE_S)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const NOW: i64 = 1_763_800_000;

    /// A reading `age_s` seconds old, stamped that long before `NOW`.
    fn ago(age_s: i64) -> LastOk {
        LastOk {
            at: NOW - age_s,
            age_s,
        }
    }

    fn read(age_s: i64) -> Reading {
        Reading {
            at: NOW - age_s,
            age_s,
        }
    }

    /// Everything set up and working, an hour after start.
    fn good() -> HealthInputs {
        HealthInputs {
            now: NOW,
            uptime_s: 3600,
            poll_interval_secs: 60,
            host_configured: true,
            connection: ConnectionState::Connected,
            reading: Some(read(10)),
            poll: PollBeat::Beat {
                at: NOW - 1,
                overdue: false,
            },
            history: Some(HistoryWrites {
                ok_at: NOW - 30,
                ..HistoryWrites::default()
            }),
            backup: BackupInput {
                enabled: true,
                enabled_for_s: 3600,
                interval_hours: 24,
                last_success_at: Some(NOW - 3600),
                last_attempt_ok: Some(true),
            },
            disk: DiskInput {
                free_bytes: Some(10 * 1024 * 1024 * 1024),
            },
            notifications: NotifyInput {
                enabled: true,
                channels: vec![NotifyMarks {
                    delivered_at: NOW - 600,
                    ..NotifyMarks::default()
                }],
            },
            weather: FetchInput {
                enabled: true,
                enabled_for_s: 3600,
                last_ok: Some(ago(300)),
            },
            solar_forecast: FetchInput {
                enabled: true,
                enabled_for_s: 3600,
                last_ok: Some(ago(3600)),
            },
            octopus: OctopusInput {
                configured: true,
                enabled_for_s: 3600,
                last_sync: Some(ago(3600)),
                failed: false,
            },
        }
    }

    fn with(f: impl FnOnce(&mut HealthInputs)) -> HealthReport {
        let mut inputs = good();
        f(&mut inputs);
        build_health(&inputs)
    }

    #[test]
    fn everything_working_is_healthy() {
        let report = build_health(&good());
        assert_eq!(report.status, Overall::Healthy);
        for check in report.checks.all() {
            assert_eq!(check.status, CheckStatus::Ok, "{check:?}");
        }
        assert_eq!(report.checks.inverter.age_s, Some(10));
        assert_eq!(report.checks.inverter.last_ok, Some(NOW - 10));
        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.uptime_s, 3600);
    }

    #[test]
    fn nothing_set_up_is_disabled_and_still_healthy() {
        let report = with(|i| {
            i.backup.enabled = false;
            i.notifications.enabled = false;
            i.weather.enabled = false;
            i.solar_forecast.enabled = false;
            i.octopus.configured = false;
        });
        assert_eq!(report.status, Overall::Healthy);
        for check in [
            &report.checks.backup,
            &report.checks.notifications,
            &report.checks.weather,
            &report.checks.solar_forecast,
            &report.checks.octopus,
        ] {
            assert_eq!(check.status, CheckStatus::Disabled);
        }
    }

    #[test]
    fn a_disabled_feature_never_counts_however_stale_its_marks() {
        let report = with(|i| {
            i.octopus.configured = false;
            i.octopus.failed = true;
            i.weather.enabled = false;
            i.weather.last_ok = Some(ago(30 * 86_400));
            i.notifications.enabled = false;
            i.notifications.channels[0].failing = true;
            i.backup.enabled = false;
            i.backup.last_attempt_ok = Some(false);
        });
        assert_eq!(report.status, Overall::Healthy);
    }

    #[test]
    fn reading_age_threshold_is_three_polls_and_at_least_three_minutes() {
        // 60 s polls: 180 s is the limit.
        let at = |age, interval| {
            with(|i| {
                i.reading = Some(read(age));
                i.poll_interval_secs = interval;
            })
        };
        assert_eq!(at(180, 60).checks.inverter.status, CheckStatus::Ok);
        let stale = at(181, 60);
        assert_eq!(stale.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(stale.checks.inverter.detail, Some("stale_reading"));
        assert_eq!(stale.status, Overall::Unhealthy);
        // 10 s polls still get three minutes.
        assert_eq!(at(180, 10).checks.inverter.status, CheckStatus::Ok);
        // 120 s polls get six.
        assert_eq!(at(360, 120).checks.inverter.status, CheckStatus::Ok);
        assert_eq!(at(361, 120).checks.inverter.status, CheckStatus::Fail);
    }

    #[test]
    fn a_long_outage_fails_the_inverter_but_not_history_and_keeps_its_age() {
        let report = with(|i| {
            i.connection = ConnectionState::Reconnecting;
            i.reading = Some(read(3600));
            // History stopped when the readings did: nothing is waiting.
            i.history = Some(HistoryWrites {
                ok_at: NOW - 3600,
                ..HistoryWrites::default()
            });
        });
        assert_eq!(report.status, Overall::Unhealthy);
        assert_eq!(report.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(report.checks.inverter.detail, Some("reconnecting"));
        assert_eq!(report.checks.inverter.last_ok, Some(NOW - 3600));
        assert_eq!(report.checks.inverter.age_s, Some(3600));
        assert_eq!(report.checks.history.status, CheckStatus::Ok);
        assert_eq!(report.checks.poll_loop.status, CheckStatus::Ok);
    }

    #[test]
    fn a_brief_reconnect_is_not_a_failure() {
        // Long after start, the dongle dropped 20 s after the last reading.
        for connection in [ConnectionState::Reconnecting, ConnectionState::Disconnected] {
            let report = with(|i| {
                i.connection = connection.clone();
                i.reading = Some(read(30));
            });
            assert_eq!(report.status, Overall::Healthy, "{connection:?}");
            assert_eq!(report.checks.inverter.status, CheckStatus::Ok);
        }
        let back = with(|i| i.reading = Some(read(170)));
        assert_eq!(back.checks.inverter.status, CheckStatus::Ok);
        assert_eq!(back.checks.inverter.detail, Some("connected"));
        // Three polls without data fails, whatever the connection says.
        let gone = with(|i| {
            i.connection = ConnectionState::Disconnected;
            i.reading = Some(read(181));
        });
        assert_eq!(gone.checks.inverter.detail, Some("disconnected"));
        assert_eq!(gone.status, Overall::Unhealthy);
    }

    #[test]
    fn no_host_is_not_configured() {
        let report = with(|i| {
            i.host_configured = false;
            i.connection = ConnectionState::Disconnected;
            i.reading = None;
        });
        assert_eq!(report.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(report.checks.inverter.detail, Some("not_configured"));
    }

    #[test]
    fn a_restart_reads_as_starting_for_five_minutes_then_fails() {
        let at = |uptime| {
            with(|i| {
                i.uptime_s = uptime;
                i.connection = ConnectionState::Disconnected;
                i.reading = None;
                i.poll = PollBeat::NotYet;
            })
        };
        let early = at(STARTUP_GRACE_S - 1);
        assert_eq!(early.status, Overall::Degraded);
        assert_eq!(early.checks.inverter.status, CheckStatus::Starting);
        assert_eq!(early.checks.poll_loop.status, CheckStatus::Starting);
        assert_eq!(early.checks.inverter.detail, Some("disconnected"));
        let late = at(STARTUP_GRACE_S);
        assert_eq!(late.status, Overall::Unhealthy);
        assert_eq!(late.checks.inverter.status, CheckStatus::Fail);
        assert_eq!(late.checks.inverter.detail, Some("no_reading"));
        assert_eq!(late.checks.poll_loop.detail, Some("not_running"));
        // Connected with no reading yet is also only starting at first.
        let connected = with(|i| {
            i.uptime_s = 30;
            i.reading = None;
        });
        assert_eq!(connected.checks.inverter.status, CheckStatus::Starting);
        assert_eq!(connected.checks.inverter.detail, Some("connected"));
    }

    #[test]
    fn an_overdue_poll_beat_is_stuck() {
        let report = with(|i| {
            i.poll = PollBeat::Beat {
                at: NOW - 1000,
                overdue: true,
            }
        });
        assert_eq!(report.status, Overall::Unhealthy);
        assert_eq!(report.checks.poll_loop.detail, Some("stuck"));
        assert_eq!(report.checks.poll_loop.age_s, Some(1000));
    }

    #[test]
    fn history_fails_when_rows_wait_too_long_or_the_last_write_failed() {
        let waiting = |secs| {
            with(|i| {
                i.history = Some(HistoryWrites {
                    ok_at: NOW - 1000,
                    waited_s: Some(secs),
                    ..HistoryWrites::default()
                })
            })
        };
        // 60 s polls: 300 s is the limit.
        assert_eq!(waiting(300).checks.history.status, CheckStatus::Ok);
        let late = waiting(301);
        assert_eq!(late.checks.history.status, CheckStatus::Fail);
        assert_eq!(late.checks.history.detail, Some("not_written"));
        assert_eq!(late.status, Overall::Unhealthy);

        let failed = with(|i| {
            i.history = Some(HistoryWrites {
                ok_at: NOW - 120,
                err_at: NOW - 60,
                waited_s: None,
                failing: true,
            })
        });
        // One failed write only degrades: the next minute's row may heal it.
        assert_eq!(failed.checks.history.status, CheckStatus::Warn);
        assert_eq!(failed.checks.history.detail, Some("write_failed"));
        assert_eq!(failed.checks.history.last_ok, Some(NOW - 120));
        assert_eq!(failed.status, Overall::Degraded);
        // Still failing once the rows have waited too long: unhealthy.
        let stuck = with(|i| {
            i.history = Some(HistoryWrites {
                ok_at: NOW - 400,
                err_at: NOW - 60,
                waited_s: Some(301),
                failing: true,
            })
        });
        assert_eq!(stuck.checks.history.detail, Some("not_written"));
        assert_eq!(stuck.status, Overall::Unhealthy);

        // A later success clears it.
        let recovered = with(|i| {
            i.history = Some(HistoryWrites {
                ok_at: NOW - 30,
                err_at: NOW - 60,
                waited_s: None,
                failing: false,
            })
        });
        assert_eq!(recovered.checks.history.status, CheckStatus::Ok);

        let missing = with(|i| i.history = None);
        assert_eq!(missing.checks.history.detail, Some("unavailable"));
        assert_eq!(missing.status, Overall::Unhealthy);

        let fresh = with(|i| i.history = Some(HistoryWrites::default()));
        assert_eq!(fresh.checks.history.status, CheckStatus::Ok);
        assert_eq!(fresh.checks.history.detail, Some("idle"));
        assert_eq!(fresh.checks.history.last_ok, None);
    }

    #[test]
    fn slow_polls_give_history_longer() {
        let report = with(|i| {
            i.poll_interval_secs = 300;
            i.history = Some(HistoryWrites {
                ok_at: NOW - 700,
                waited_s: Some(720),
                ..HistoryWrites::default()
            });
        });
        assert_eq!(report.checks.history.status, CheckStatus::Ok);
    }

    #[test]
    fn non_core_failures_only_degrade() {
        let report = with(|i| {
            i.backup.last_attempt_ok = Some(false);
            i.disk.free_bytes = Some(10 * 1024 * 1024);
            i.notifications.channels[0].failing = true;
            i.weather.last_ok = Some(ago(86_400));
            i.octopus.failed = true;
        });
        assert_eq!(report.status, Overall::Degraded);
        assert_eq!(report.checks.disk.status, CheckStatus::Fail);
        assert_eq!(report.checks.backup.detail, Some("last_failed"));
        assert_eq!(report.checks.notifications.detail, Some("last_failed"));
        assert_eq!(report.checks.weather.detail, Some("stale"));
        assert_eq!(report.checks.octopus.detail, Some("last_failed"));
    }

    #[test]
    fn backup_boundaries() {
        let at = |f: fn(&mut BackupInput)| {
            let mut i = good();
            f(&mut i.backup);
            build_health(&i).checks.backup
        };
        // Daily: overdue after 49 h.
        let ok = at(|b| b.last_success_at = Some(NOW - 49 * 3600));
        assert_eq!(ok.status, CheckStatus::Ok);
        let overdue = at(|b| b.last_success_at = Some(NOW - 49 * 3600 - 1));
        assert_eq!(overdue.detail, Some("overdue"));
        // Never: fine until backups have been on one interval and an hour.
        let mut i = good();
        i.backup.last_success_at = None;
        i.backup.last_attempt_ok = None;
        i.backup.enabled_for_s = 25 * 3600;
        i.uptime_s = 30 * 86_400;
        assert_eq!(build_health(&i).checks.backup.detail, Some("not_yet"));
        i.backup.enabled_for_s = 25 * 3600 + 1;
        assert_eq!(build_health(&i).checks.backup.detail, Some("never"));
        // A failed manual attempt counts as much as a scheduled one.
        let failed = at(|b| b.last_attempt_ok = Some(false));
        assert_eq!(failed.detail, Some("last_failed"));
    }

    #[test]
    fn disk_boundaries() {
        const MB: u64 = 1024 * 1024;
        let at = |free| {
            build_health(&HealthInputs {
                disk: DiskInput {
                    free_bytes: Some(free),
                },
                ..good()
            })
            .checks
            .disk
        };
        assert_eq!(at(500 * MB).status, CheckStatus::Ok);
        assert_eq!(at(500 * MB - 1).detail, Some("low_space"));
        assert_eq!(at(100 * MB).detail, Some("low_space"));
        assert_eq!(at(100 * MB - 1).detail, Some("very_low_space"));
        assert_eq!(at(2048 * MB).free_mb, Some(2048));
        let unknown = build_health(&HealthInputs {
            disk: DiskInput { free_bytes: None },
            ..good()
        });
        assert_eq!(unknown.checks.disk.detail, Some("unavailable"));
        assert_eq!(unknown.status, Overall::Healthy);
    }

    #[test]
    fn notifications_recover_on_a_later_delivery() {
        let failed = with(|i| i.notifications.channels[0].failing = true);
        assert_eq!(failed.checks.notifications.status, CheckStatus::Warn);
        assert_eq!(failed.checks.notifications.last_ok, Some(NOW - 600));
        let marks = DeliveryMarks::new();
        marks.record(1, false, NOW - 300);
        assert!(marks.snapshot().failing);
        marks.record(1, true, NOW - 100);
        let recovered = with(|i| i.notifications.channels = vec![marks.snapshot()]);
        assert_eq!(recovered.checks.notifications.status, CheckStatus::Ok);
        let none = with(|i| i.notifications.channels[0].delivered_at = 0);
        assert_eq!(none.checks.notifications.detail, Some("none_sent"));
        assert_eq!(none.status, Overall::Healthy);
    }

    #[test]
    fn one_dead_channel_shows_whichever_order_they_send_in() {
        let working = NotifyMarks {
            delivered_at: NOW - 60,
            ..NotifyMarks::default()
        };
        let dead = NotifyMarks {
            failed_at: NOW - 60,
            failing: true,
            ..NotifyMarks::default()
        };
        for channels in [vec![working, dead], vec![dead, working]] {
            let report = with(|i| i.notifications.channels = channels);
            assert_eq!(report.checks.notifications.detail, Some("last_failed"));
            assert_eq!(report.checks.notifications.last_ok, Some(NOW - 60));
        }
    }

    #[test]
    fn fetch_boundaries() {
        let weather = |age| with(|i| i.weather.last_ok = Some(ago(age))).checks.weather;
        assert_eq!(weather(WEATHER_STALE_S).status, CheckStatus::Ok);
        assert_eq!(weather(WEATHER_STALE_S + 1).detail, Some("stale"));
        let forecast = |age| {
            with(|i| i.solar_forecast.last_ok = Some(ago(age)))
                .checks
                .solar_forecast
        };
        assert_eq!(forecast(SOLAR_FORECAST_STALE_S).status, CheckStatus::Ok);
        assert_eq!(forecast(SOLAR_FORECAST_STALE_S + 1).detail, Some("stale"));
        let octopus = |age| {
            with(|i| i.octopus.last_sync = Some(ago(age)))
                .checks
                .octopus
        };
        assert_eq!(octopus(OCTOPUS_STALE_S).status, CheckStatus::Ok);
        assert_eq!(octopus(OCTOPUS_STALE_S + 1).detail, Some("stale"));
        // Never fetched: fine until the feature has been on as long as a
        // fetch takes to go stale, however long HEM has been running.
        let never = |enabled_for_s| {
            with(|i| {
                i.uptime_s = 30 * 86_400;
                i.solar_forecast.enabled_for_s = enabled_for_s;
                i.solar_forecast.last_ok = None;
            })
            .checks
            .solar_forecast
        };
        assert_eq!(never(SOLAR_FORECAST_STALE_S).detail, Some("not_yet"));
        assert_eq!(never(SOLAR_FORECAST_STALE_S + 1).detail, Some("never"));
    }

    #[test]
    fn a_backwards_clock_never_gives_a_negative_age() {
        let report = with(|i| {
            i.reading = Some(Reading {
                at: NOW + 120,
                age_s: 0,
            });
            // Fetched when the clock was ten minutes ahead, then stepped
            // back: shown at its own time, aged on the monotonic clock.
            i.weather.last_ok = Some(LastOk {
                at: NOW + 600,
                age_s: 60,
            });
        });
        assert_eq!(report.checks.inverter.age_s, Some(0));
        assert_eq!(report.checks.inverter.status, CheckStatus::Ok);
        assert_eq!(report.checks.weather.last_ok, Some(NOW + 600));
        assert_eq!(report.checks.weather.age_s, Some(60));
    }

    #[test]
    fn a_fetch_is_judged_on_its_monotonic_age_not_the_wall_clock() {
        // A Pi boots with its clock two days behind, fetches, then NTP steps
        // the clock forward: the fetch is a minute old, not two days.
        let report = with(|i| {
            i.weather.last_ok = Some(LastOk {
                at: NOW - 2 * 86_400,
                age_s: 60,
            })
        });
        assert_eq!(report.checks.weather.status, CheckStatus::Ok);
        assert_eq!(report.checks.weather.age_s, Some(60));
        // And a clock stepped back can't hide a stale one.
        let report = with(|i| {
            i.weather.last_ok = Some(LastOk {
                at: NOW + 86_400,
                age_s: WEATHER_STALE_S + 1,
            })
        });
        assert_eq!(report.checks.weather.detail, Some("stale"));
    }

    #[test]
    fn a_feature_switched_back_on_gets_its_grace_despite_an_old_success() {
        let reenabled = |enabled_for_s| {
            with(|i| {
                i.uptime_s = 30 * 86_400;
                i.weather.enabled_for_s = enabled_for_s;
                i.weather.last_ok = Some(ago(86_400));
            })
            .checks
            .weather
        };
        assert_eq!(reenabled(0).detail, Some("not_yet"));
        assert_eq!(reenabled(WEATHER_STALE_S).detail, Some("not_yet"));
        assert_eq!(reenabled(WEATHER_STALE_S + 1).detail, Some("stale"));
        // The old fetch is still shown.
        assert_eq!(reenabled(0).last_ok, Some(NOW - 86_400));

        // Backups off for a month and back on: not overdue straight away.
        let backup = |enabled_for_s, uptime_s| {
            with(|i| {
                i.uptime_s = uptime_s;
                i.backup.enabled_for_s = enabled_for_s;
                i.backup.last_success_at = Some(NOW - 30 * 86_400);
            })
            .checks
            .backup
        };
        assert_eq!(backup(600, 30 * 86_400).detail, Some("not_yet"));
        assert_eq!(backup(25 * 3600 + 1, 30 * 86_400).detail, Some("overdue"));
        // On since start: a restart doesn't hide an overdue backup.
        assert_eq!(backup(600, 600).detail, Some("overdue"));
    }

    #[test]
    fn the_report_carries_only_the_pinned_keys() {
        let mut i = good();
        i.disk.free_bytes = Some(1);
        let v = serde_json::to_value(build_health(&i)).unwrap();
        let keys = |v: &Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&v), ["checks", "now", "status", "uptime_s", "version"]);
        assert_eq!(
            keys(&v["checks"]),
            [
                "backup",
                "disk",
                "history",
                "inverter",
                "notifications",
                "octopus",
                "poll_loop",
                "solar_forecast",
                "weather"
            ]
        );
        let allowed = ["age_s", "core", "detail", "free_mb", "last_ok", "status"];
        for (name, check) in v["checks"].as_object().unwrap() {
            for key in keys(check) {
                assert!(allowed.contains(&key.as_str()), "{name}.{key}");
            }
            if let Some(detail) = check.get("detail") {
                let d = detail.as_str().unwrap();
                assert!(
                    d.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                    "{name}.detail must be a code, got {d:?}"
                );
            }
        }
        assert_eq!(v["status"], "degraded");
        assert_eq!(v["checks"]["disk"]["status"], "fail");
        assert_eq!(v["checks"]["inverter"]["core"], true);
    }

    #[test]
    fn poll_marks_go_overdue_only_after_their_budget() {
        let start = Instant::now();
        let marks = HealthMarks::new(start);
        assert_eq!(marks.poll_state(start), PollBeat::NotYet);
        marks.poll_beat_at_time(start, NOW, Duration::from_secs(120));
        let at = |s| marks.poll_state(start + Duration::from_secs(s));
        assert_eq!(
            at(120),
            PollBeat::Beat {
                at: NOW,
                overdue: false
            }
        );
        assert_eq!(
            at(121),
            PollBeat::Beat {
                at: NOW,
                overdue: true
            }
        );
        // A fresh beat resets the budget.
        marks.poll_beat_at_time(
            start + Duration::from_secs(200),
            NOW + 200,
            POLL_IDLE_BUDGET,
        );
        assert_eq!(
            at(300),
            PollBeat::Beat {
                at: NOW + 200,
                overdue: false
            }
        );
        assert_eq!(marks.uptime_s(start + Duration::from_secs(300)), 300);
        // An instant before start (can't happen, but) is never negative.
        assert_eq!(marks.uptime_s(start), 0);
    }

    #[test]
    fn the_last_reading_ages_on_the_monotonic_clock() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = HealthMarks::new(start);
        assert_eq!(marks.reading(at(10)), None);
        marks.note_reading_at(at(100), NOW);
        assert_eq!(marks.reading(at(130)), Some(Reading { at: NOW, age_s: 30 }));
        // The wall stamp is only shown: a reading stamped while the clock ran
        // an hour ahead still ages from when it was published, so a dead
        // dongle shows up after the clock is stepped back.
        marks.note_reading_at(at(200), NOW + 3600);
        assert_eq!(
            marks.reading(at(500)),
            Some(Reading {
                at: NOW + 3600,
                age_s: 300
            })
        );
    }

    #[test]
    fn a_running_poll_cycle_stops_the_reading_from_ageing() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = HealthMarks::new(start);
        marks.note_reading_at(at(100), NOW);
        // The next cycle starts 60 s later and spends minutes draining writes
        // the dongle keeps refusing: the reading counts as 60 s old until the
        // cycle ends (its own budget is the poll check's business).
        marks.poll_cycle_start_at(at(160), NOW + 60);
        assert_eq!(marks.reading(at(400)).unwrap().age_s, 60);
        assert_eq!(
            marks.poll_state(at(400)),
            PollBeat::Beat {
                at: NOW + 60,
                overdue: false
            }
        );
        // Back to waiting without a new reading: ageing resumes.
        marks.poll_beat_at_time(at(410), NOW + 310, POLL_IDLE_BUDGET);
        assert_eq!(marks.reading(at(420)).unwrap().age_s, 320);
        // A cycle that publishes a reading counts from that reading.
        marks.poll_cycle_start_at(at(500), NOW + 400);
        marks.note_reading_at(at(505), NOW + 405);
        assert_eq!(marks.reading(at(600)).unwrap().age_s, 95);
        // The busy budget still catches a cycle that never ends.
        assert!(matches!(
            marks.poll_state(at(500) + POLL_BUSY_BUDGET + Duration::from_secs(1)),
            PollBeat::Beat { overdue: true, .. }
        ));
    }

    #[test]
    fn a_cycle_past_its_budget_no_longer_holds_the_age() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let budget = POLL_BUSY_BUDGET.as_secs();
        let marks = HealthMarks::new(start);
        marks.note_reading_at(at(100), NOW);
        marks.poll_cycle_start_at(at(160), NOW + 60);
        // Held right up to the end of the budget...
        assert_eq!(marks.reading(at(160 + budget)).unwrap().age_s, 60);
        // ...then the cycle is stuck, and the age is the real one, so a
        // display of "no data for N minutes" tells the truth about a hang.
        assert_eq!(
            marks.reading(at(160 + budget + 1)).unwrap().age_s,
            i64::try_from(60 + budget + 1).unwrap()
        );
        assert_eq!(marks.reading(at(3 * 3600)).unwrap().age_s, 3 * 3600 - 100);
    }

    #[test]
    fn a_feature_counts_from_start_or_from_when_it_was_switched_on() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = HealthMarks::with_features_on(
            start,
            &[(Feature::Weather, true), (Feature::Backup, false)],
        );
        // On at start: counted from start, on the monotonic clock, so a wall
        // clock corrected by weeks after boot doesn't matter.
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, at(3600)), 3600);
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, at(3660)), 3660);
        // Off at the first look, switched on later: counted from then.
        assert_eq!(marks.enabled_for_s(Feature::Octopus, false, at(3600)), 0);
        assert_eq!(marks.enabled_for_s(Feature::Octopus, true, at(3700)), 0);
        assert_eq!(marks.enabled_for_s(Feature::Octopus, true, at(4000)), 300);
        // Off at start and switched on before the monitor's first look (Octopus
        // details entered, then Uptime Kuma set up): counted from that look,
        // not from start, so it gets its grace.
        assert_eq!(marks.enabled_for_s(Feature::Backup, true, at(7200)), 0);
        assert_eq!(
            marks.enabled_for_s(Feature::SolarForecast, true, at(7200)),
            0
        );
        // Off and on again restarts it.
        assert_eq!(marks.enabled_for_s(Feature::Weather, false, at(4100)), 0);
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, at(4200)), 0);
        assert_eq!(marks.enabled_for_s(Feature::Weather, true, at(4260)), 60);
    }

    #[test]
    fn write_marks_keep_the_earliest_wait_and_clear_when_written() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = WriteMarks::default();
        marks.rows_ready(at(100));
        marks.rows_ready(at(160));
        assert_eq!(marks.snapshot(at(200)).waited_s, Some(100));
        marks.write_ok(170, false);
        assert_eq!(
            marks.snapshot(at(200)),
            HistoryWrites {
                ok_at: 170,
                ..HistoryWrites::default()
            }
        );
        marks.rows_ready(at(300));
        marks.write_failed(205);
        // Failing, the wait runs to the latest ready rows, not to now.
        assert_eq!(marks.snapshot(at(310)).waited_s, Some(0));
        assert_eq!(marks.snapshot(at(310)).err_at, 205);
        assert!(marks.snapshot(at(310)).failing);
        // A success after the clock steps back still clears the failure, and
        // its own (earlier) time is what's shown.
        marks.write_ok(150, false);
        assert!(!marks.snapshot(at(310)).failing);
        assert_eq!(marks.snapshot(at(310)).ok_at, 150);
    }

    #[test]
    fn a_failed_write_then_a_dongle_outage_does_not_blame_history() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = WriteMarks::default();
        marks.rows_ready(at(100));
        marks.write_failed(NOW);
        // The dongle drops: no rows come for an hour, so nothing waits.
        assert_eq!(marks.snapshot(at(3700)).waited_s, Some(0));
        // It comes back and the next write works: all clear.
        marks.rows_ready(at(3760));
        marks.write_ok(NOW + 3700, false);
        assert_eq!(marks.snapshot(at(3770)).waited_s, None);
    }

    #[test]
    fn writes_that_keep_failing_while_rows_keep_coming_run_out_the_wait() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = WriteMarks::default();
        for minute in 0..7 {
            marks.rows_ready(at(100 + minute * 60));
            marks.write_failed(NOW + i64::try_from(minute * 60).unwrap());
        }
        assert_eq!(marks.snapshot(at(470)).waited_s, Some(360));
        // A writer that hangs (no result at all) waits on the clock as before.
        let hung = WriteMarks::default();
        hung.rows_ready(at(100));
        assert_eq!(hung.snapshot(at(500)).waited_s, Some(400));
    }

    #[test]
    fn rows_queued_during_a_write_count_from_when_they_became_ready() {
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let marks = WriteMarks::default();
        marks.rows_ready(at(100));
        marks.write_failed(NOW);
        // An hour later rows come again and a write works, with one more
        // row queued meanwhile: it hasn't waited an hour.
        marks.rows_ready(at(3700));
        marks.write_ok(NOW + 3600, true);
        assert_eq!(marks.snapshot(at(3710)).waited_s, Some(10));
    }

    #[test]
    fn a_reset_channel_starts_afresh() {
        let marks = DeliveryMarks::new();
        marks.record(1, true, 100);
        marks.record(1, false, 120);
        marks.reset(2);
        assert_eq!(marks.snapshot(), NotifyMarks::default());
    }

    #[test]
    fn a_send_with_the_old_credentials_after_a_reset_is_ignored() {
        let marks = DeliveryMarks::new();
        marks.record(1, false, 100);
        // The settings are fixed while a send with the old ones is in flight.
        marks.reset(2);
        marks.record(1, false, 110);
        assert_eq!(marks.snapshot(), NotifyMarks::default());
        marks.record(2, true, 120);
        assert_eq!(marks.snapshot().delivered_at, 120);
    }

    #[test]
    fn credentials_have_a_stable_fingerprint() {
        assert_eq!(credentials_key(&["t", "c"]), credentials_key(&["t", "c"]));
        assert_ne!(credentials_key(&["t", "c"]), credentials_key(&["t", "d"]));
        // Field boundaries count: "ab"+"c" is not "a"+"bc".
        assert_ne!(credentials_key(&["ab", "c"]), credentials_key(&["a", "bc"]));
    }

    #[test]
    fn delivery_marks_keep_the_latest_of_each() {
        let marks = DeliveryMarks::new();
        marks.record(1, true, 100);
        marks.record(1, false, 120);
        assert!(marks.snapshot().failing);
        // A delivery after the clock stepped back still clears the failure.
        marks.record(1, true, 90);
        assert_eq!(
            marks.snapshot(),
            NotifyMarks {
                delivered_at: 90,
                failed_at: 120,
                failing: false
            }
        );
    }
}

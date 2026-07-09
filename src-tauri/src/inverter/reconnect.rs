//! Reconnect / back-off state machine for the inverter poll loop.
//!
//! Extracted from `run_poll_loop` so the multi-session reconnect logic —
//! sustained-timeout disconnect, dead-session escalation, flap gate, and the
//! connect-failure exponential — is unit-testable as a *driven* state machine,
//! not only via the pure helper functions. [`ReconnectController`] owns every
//! piece of state that decides whether to disconnect mid-session and how long
//! to wait before reconnecting; `run_poll_loop` is now a thin caller that feeds
//! it events (`note_session_start`, `note_good_poll`, `note_poll_failed`, …).
//!
//! Time is injected (`Instant` parameters) so the session-simulation tests are
//! fully deterministic — no wall-clock dependence, no real sleeps. The
//! production caller passes `Instant::now()`.
//!
//! ## What is NOT owned here
//!
//! The auto-discovery subsystem (`consecutive_connect_failures`,
//! `last_discovery_time`, LAN scanning) is a separate concern and stays in
//! `run_poll_loop`. [`ReconnectController::reset_connect_backoff`] is the seam
//! the discovery path uses to give an auto-switched host a fresh fast attempt.

use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Back-off schedules (pure helpers)
// ---------------------------------------------------------------------------

/// Reconnect delay for sessions that connected but produced no successful
/// Modbus reads ("zombie dongle"). Escalates with consecutive dead sessions
/// so a chronically hung dongle is given time to self-recover instead of
/// being hammered in a tight reconnect loop. TCP `connect()` succeeds for a
/// zombie, so the normal connect-failure back-off never kicks in — this
/// function provides the escalation that path is missing.
///
/// Schedule: a small flat ramp capped low (0–1 → 5 s, 2+ → 10 s).
///
/// GivTCP reconnects on a dead/zombie session with a flat ~2 s pause
/// (`read.py`: `close()` → `sleep(2)` → `connect()`) and only escalates to a
/// full container restart after 10 *hard* connect failures (which HEM maps
/// to auto-discovery instead). The previous schedule here (5 → 30 → 60 →
/// 120 → 300 → 600 s) parked a self-healing dongle behind a 10-minute wall
/// after a handful of zombie sessions — during which the dongle typically
/// recovers on its own. A small ramp that caps low gives a genuinely-wedged
/// dongle modest breathing room without stranding a recovering one for
/// minutes. (The separate connect-failure back-off only bites on actual
/// `connect()` failures, where it is appropriate to back off harder.)
fn dead_session_backoff(consecutive_dead_sessions: u32) -> Duration {
    match consecutive_dead_sessions {
        0 | 1 => Duration::from_secs(5),
        _ => Duration::from_secs(10),
    }
}

/// Flap back-off: a third reconnect-delay gate, calibrated for a *flapping*
/// dongle rather than a chronically-dead one.
///
/// Both [`dead_session_backoff`] (reset on a single successful poll) and the
/// connect-failure exponential (reset on a successful TCP `connect()`) are easy
/// to reset during a flap — a dongle that answers a read or two every minute or
/// two, keeping the UI pinned to "Reconnecting" for a quarter hour while HEM
/// storms it with reconnects every few seconds. Neither timer measures the
/// thing that actually matters during a flap: *has the frontend received
/// fresh data recently?*
///
/// This gate does. The controller tracks `last_good_data_at` (updated only on a
/// fully-delivered, sanitized snapshot) and engages an elevated reconnect
/// delay once the gap exceeds [`FLAP_THRESHOLD_SECS`]. The engaged state is
/// **sticky**: it stands down only after [`FLAP_STANDDOWN_POLLS`] consecutive
/// good polls, so a single isolated success mid-flap can't yo-yo the cadence
/// back to fast. Entry is fast (one data-starved interval); exit deliberately
/// requires proof of sustained recovery.
///
/// The elevated delay is deliberately above the [`dead_session_backoff`] cap
/// (10 s) and the connect back-off floor (5 s) so that once engaged it actually
/// wins the `Duration::max` selection — the other gates stay in place for the
/// genuinely-chronic cases they handle.
const FLAP_THRESHOLD_SECS: u64 = 120;
const FLAP_ELEVATED_DELAY: Duration = Duration::from_secs(30);
const FLAP_STANDDOWN_POLLS: u8 = 3;

/// Whether the flap gate should engage given the time (in seconds) since the
/// last fully-delivered good snapshot.
fn flap_should_engage(secs_since_good_data: u64) -> bool {
    secs_since_good_data >= FLAP_THRESHOLD_SECS
}

/// Reconnect delay contributed by the flap gate. Zero when disengaged so the
/// other back-off gates decide the delay; [`FLAP_ELEVATED_DELAY`] when engaged.
fn flap_backoff(engaged: bool) -> Duration {
    if engaged {
        FLAP_ELEVATED_DELAY
    } else {
        Duration::ZERO
    }
}

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// After this many consecutive all-block-timeout poll cycles within one
/// session, force a reconnect. 3 cycles × ~12 s each (3 s × 3 attempts per
/// block + inter-request delay + the post-poll 2 s sleep) ≈ 36 s of sustained
/// silence before we give up — long enough to ride out a brief dongle hiccup,
/// short enough to recover well before the 5–10 minute TCP RST that would
/// otherwise arrive.
///
/// Lives on the controller (not as a local inside `run_poll_loop`) so it is
/// reachable from a unit test — the original local-const placement meant the
/// disconnect threshold could only be "tested" via compile-time timing math.
const MAX_CONSECUTIVE_TIMEOUTS: u8 = 3;

/// Connect-failure exponential back-off floor (also the post-successful-connect
/// reset value) and ceiling.
const CONNECT_BACKOFF_FLOOR: Duration = Duration::from_secs(5);
const CONNECT_BACKOFF_CAP: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Owns every piece of reconnect/back-off state for [`super::poll::run_poll_loop`].
///
/// Constructed once before the outer loop; the loop drives it through a
/// sequence of `note_*` events and reads the resulting delay from
/// [`reconnect_delay`](Self::reconnect_delay). See the [module docs](self)
/// for the division of labour with the auto-discovery subsystem.
///
/// Behaviour is a straight lift of the mutable locals that used to live inline
/// in `run_poll_loop`; the session-simulation tests in this module pin the
/// *transitions* between them (which the pure helper tests could not reach).
pub(crate) struct ReconnectController {
    /// Exponential back-off for TCP `connect()` failures. Reset to the floor
    /// on a successful connect; doubled (capped) after each reconnect sleep.
    backoff: Duration,
    /// Consecutive sessions that connected (TCP up) but produced ZERO good
    /// reads — the "zombie dongle" signature. Drives [`dead_session_backoff`].
    /// Reset to 0 the moment a session yields at least one good poll.
    consecutive_dead_sessions: u32,
    /// Wall-clock of the last fully-delivered, sanitized snapshot. Drives the
    /// flap gate's data-starvation check.
    last_good_data_at: Instant,
    /// Flap gate engaged state. Sticky: stands down only after
    /// [`FLAP_STANDDOWN_POLLS`] consecutive good polls.
    flap_engaged: bool,
    /// Consecutive good polls while the flap is engaged (drives stand-down).
    consecutive_good_polls: u8,
    /// Snapshot of the manual-reconnect request counter (`POST /api/reconnect`).
    last_seen_reconnect_request: u32,
    /// Consecutive transient-timeout poll cycles within the current session.
    consecutive_timeouts: u8,
    /// Whether the current session has delivered at least one good poll.
    /// Drives the dead-session tally at [`note_session_end`](Self::note_session_end).
    session_had_good_read: bool,
}

impl ReconnectController {
    /// New controller. `now` seeds the flap data-starvation clock; the
    /// manual-reconnect snapshot is seeded with `initial_request_counter` so
    /// the very first [`check_manual_reconnect`](Self::check_manual_reconnect)
    /// does not mistake the current counter for a fresh request.
    pub(crate) fn new(now: Instant, initial_request_counter: u32) -> Self {
        Self {
            backoff: CONNECT_BACKOFF_FLOOR,
            consecutive_dead_sessions: 0,
            last_good_data_at: now,
            flap_engaged: false,
            consecutive_good_polls: 0,
            last_seen_reconnect_request: initial_request_counter,
            consecutive_timeouts: 0,
            session_had_good_read: false,
        }
    }

    /// Detect a manual `POST /api/reconnect` by comparing the current request
    /// counter to the last one we saw. On a change, reset *every* gate to the
    /// fast-retry state so the user's click actually retries quickly rather
    /// than being swallowed by a long zombie-dongle back-off sleep. Returns
    /// `true` iff a reset occurred.
    pub(crate) fn check_manual_reconnect(
        &mut self,
        current_request_counter: u32,
        now: Instant,
    ) -> bool {
        if current_request_counter != self.last_seen_reconnect_request {
            tracing::info!(
                from = self.last_seen_reconnect_request,
                to = current_request_counter,
                "Manual reconnect requested — resetting back-off state"
            );
            self.backoff = CONNECT_BACKOFF_FLOOR;
            self.consecutive_dead_sessions = 0;
            self.flap_engaged = false;
            self.consecutive_good_polls = 0;
            self.last_good_data_at = now;
            self.last_seen_reconnect_request = current_request_counter;
            true
        } else {
            false
        }
    }

    /// The auto-discovery path switched to a new host; reset the connect
    /// back-off so the new host gets a fresh fast TCP attempt instead of a
    /// stale escalated one.
    pub(crate) fn reset_connect_backoff(&mut self) {
        self.backoff = CONNECT_BACKOFF_FLOOR;
    }

    /// A TCP `connect()` succeeded — begin a new session. Resets the
    /// per-session timeout streak (a fresh start) and the connect back-off
    /// (the network path is good). Does **not** reset dead-session or flap
    /// state: those are deliberately cross-session (a flap is a multi-session
    /// phenomenon).
    pub(crate) fn note_session_start(&mut self) {
        self.backoff = CONNECT_BACKOFF_FLOOR;
        self.consecutive_timeouts = 0;
        self.session_had_good_read = false;
    }

    /// A poll cycle delivered a good, sanitized snapshot. Resets the
    /// sustained-timeout streak, marks the session productive, restarts the
    /// flap data-starvation clock, and — if a flap is engaged — advances the
    /// stand-down count, disengaging after [`FLAP_STANDDOWN_POLLS`] good
    /// polls in a row.
    pub(crate) fn note_good_poll(&mut self, now: Instant) {
        self.consecutive_timeouts = 0;
        self.session_had_good_read = true;
        self.last_good_data_at = now;
        if self.flap_engaged {
            self.consecutive_good_polls += 1;
            if self.consecutive_good_polls >= FLAP_STANDDOWN_POLLS {
                self.flap_engaged = false;
                self.consecutive_good_polls = 0;
                tracing::info!(
                    polls = FLAP_STANDDOWN_POLLS,
                    "Dongle recovered — standing down flap back-off, resuming normal reconnect cadence"
                );
            }
        }
    }

    /// A poll cycle failed. Breaks the flap stand-down recovery streak — a
    /// single failed poll aborts a partial stand-down so an isolated success
    /// mid-flap can't yo-yo the cadence back to fast.
    pub(crate) fn note_poll_failed(&mut self) {
        self.consecutive_good_polls = 0;
    }

    /// Count one transient-timeout poll cycle (connection still up but the
    /// dongle never answered within the I/O timeout). Returns `true` once
    /// [`MAX_CONSECUTIVE_TIMEOUTS`] is reached — the caller's signal to
    /// disconnect and force a reconnect instead of hammering a wedged dongle
    /// until the OS sends an RST.
    pub(crate) fn note_transient_timeout(&mut self) -> bool {
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        if self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
            tracing::warn!(
                consecutive = self.consecutive_timeouts,
                max = MAX_CONSECUTIVE_TIMEOUTS,
                "Sustained Modbus timeouts - disconnecting to force reconnect"
            );
            true
        } else {
            false
        }
    }

    /// The session ended (disconnect). Tally it as productive (resets the
    /// dead-session escalation) or dead (increments it), based on whether the
    /// session ever delivered a good poll via [`note_good_poll`](Self::note_good_poll).
    pub(crate) fn note_session_end(&mut self) {
        if self.session_had_good_read {
            self.consecutive_dead_sessions = 0;
        } else {
            self.consecutive_dead_sessions = self.consecutive_dead_sessions.saturating_add(1);
            tracing::warn!(
                consecutive_dead_sessions = self.consecutive_dead_sessions,
                "Session produced no successful Modbus reads - escalating reconnect back-off"
            );
        }
    }

    /// Recompute the flap gate from the time since the last good data and
    /// return the reconnect delay — the `max` of the connect-failure back-off,
    /// the dead-session back-off, and the flap back-off. Engages the flap
    /// (sticky) if the frontend has been data-starved past
    /// [`FLAP_THRESHOLD_SECS`]. Call after [`note_session_end`](Self::note_session_end),
    /// before sleeping.
    pub(crate) fn reconnect_delay(&mut self, now: Instant) -> Duration {
        let secs_since_good_data = now.duration_since(self.last_good_data_at).as_secs();
        if !self.flap_engaged && flap_should_engage(secs_since_good_data) {
            self.flap_engaged = true;
            self.consecutive_good_polls = 0;
            tracing::warn!(
                secs_since_good_data,
                threshold = FLAP_THRESHOLD_SECS,
                "Dongle flap detected — no good data for {secs_since_good_data}s, slowing reconnect cadence"
            );
        }
        let delay = self
            .backoff
            .max(dead_session_backoff(self.consecutive_dead_sessions))
            .max(flap_backoff(self.flap_engaged));
        tracing::debug!(
            "Retrying connection in {:?} (dead_sessions={}, flap_engaged={})",
            delay,
            self.consecutive_dead_sessions,
            self.flap_engaged
        );
        delay
    }

    /// Exponentially increase the connect-failure back-off for the next outer
    /// iteration, capped at [`CONNECT_BACKOFF_CAP`]. Called after the reconnect
    /// sleep completes.
    pub(crate) fn escalate_connect_backoff(&mut self) {
        self.backoff = (self.backoff * 2).min(CONNECT_BACKOFF_CAP);
    }

    /// Last-seen manual-reconnect counter. The reconnect-sleep wake loop reads
    /// this to detect a request that arrived mid-sleep and break early (the
    /// full reset then happens via [`check_manual_reconnect`](Self::check_manual_reconnect)
    /// at the top of the next iteration).
    pub(crate) fn last_seen_reconnect_request(&self) -> u32 {
        self.last_seen_reconnect_request
    }

    /// Whether the flap gate is currently engaged. Test/diagnostics accessor;
    /// production code acts on it via [`reconnect_delay`](Self::reconnect_delay).
    #[cfg(test)]
    pub(crate) fn is_flap_engaged(&self) -> bool {
        self.flap_engaged
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed epoch for deterministic time-based tests. All derived timestamps
    /// are `now() + Duration`, which keeps them monotonic for `duration_since`.
    fn now() -> Instant {
        Instant::now()
    }

    // -------------------------------------------------------------------
    // Pure-helper schedule tests (moved here from poll.rs — the helpers now
    // live in this module alongside the controller that uses them).
    // -------------------------------------------------------------------

    #[test]
    fn dead_session_backoff_schedule() {
        // Flat, low cap — mirrors GivTCP's ~2 s reconnect on a zombie session
        // (close → sleep(2) → connect). A self-healing dongle gets another
        // shot within seconds instead of being parked behind a 10-minute wall.
        assert_eq!(dead_session_backoff(0), Duration::from_secs(5));
        assert_eq!(dead_session_backoff(1), Duration::from_secs(5));
        // 2+ caps at 10 s.
        assert_eq!(dead_session_backoff(2), Duration::from_secs(10));
        assert_eq!(dead_session_backoff(3), Duration::from_secs(10));
        assert_eq!(dead_session_backoff(5), Duration::from_secs(10));
        assert_eq!(dead_session_backoff(100), Duration::from_secs(10));
    }

    #[test]
    fn flap_should_engage_at_threshold() {
        // Below the threshold the gate stays disengaged — normal operation,
        // the existing back-off gates decide the delay.
        assert!(!flap_should_engage(0));
        assert!(!flap_should_engage(FLAP_THRESHOLD_SECS - 1));
        // At and above the threshold it engages.
        assert!(flap_should_engage(FLAP_THRESHOLD_SECS));
        assert!(flap_should_engage(FLAP_THRESHOLD_SECS + 1));
        assert!(flap_should_engage(u64::MAX));
    }

    #[test]
    fn flap_backoff_delay_schedule() {
        // Disengaged contributes nothing — the other gates win the `max`.
        assert_eq!(flap_backoff(false), Duration::ZERO);
        // Engaged contributes a delay above the dead-session cap (10 s) and
        // the connect back-off floor (5 s) so it actually wins the `max`
        // selection rather than being shadowed by the other gates.
        let engaged = flap_backoff(true);
        assert_eq!(engaged, FLAP_ELEVATED_DELAY);
        assert!(engaged > Duration::from_secs(10));
        assert!(engaged > Duration::from_secs(5));
    }

    /// The flap gate's design contract. The integer-constant checks run at
    /// compile time so a tweak to any constant trips the build immediately;
    /// the `Duration` checks run at runtime. The stand-down must demand a
    /// genuine run of success (not a single poll) to avoid yo-yoing, and the
    /// threshold must be long enough that a healthy (if slow) poll interval
    /// never trips it.
    #[test]
    fn flap_gate_contract_holds() {
        const _: () = {
            assert!(
                FLAP_STANDDOWN_POLLS >= 2,
                "stand-down must require a sustained run, not a single poll",
            );
            assert!(
                FLAP_THRESHOLD_SECS > 60,
                "threshold must exceed a slow poll interval so the gate never engages on healthy operation",
            );
        };
        // The elevated delay must dominate the other two gates' maximums to
        // have any effect under `Duration::max`.
        assert!(
            FLAP_ELEVATED_DELAY > Duration::from_secs(10),
            "flap delay must exceed dead-session cap"
        );
        assert!(
            FLAP_ELEVATED_DELAY > Duration::from_secs(5),
            "flap delay must exceed connect back-off floor"
        );
    }

    // -------------------------------------------------------------------
    // Session-simulation tests (the gap these close: the *transitions*
    // between the back-off gates, driven as a state machine rather than
    // tested piecemeal via the pure helpers).
    // -------------------------------------------------------------------

    /// `MAX_CONSECUTIVE_TIMEOUTS` is now reachable: three consecutive transient
    /// timeouts within a session trip the disconnect; two do not. This replaces
    /// the const-assertion-only timing test the original local-const placement
    /// forced on us with a real behavioural check.
    #[test]
    fn sustained_timeouts_force_disconnect_only_at_threshold() {
        let mut rc = ReconnectController::new(now(), 0);
        rc.note_session_start();

        // Two transient timeouts stay under the threshold.
        rc.note_poll_failed();
        assert!(!rc.note_transient_timeout(), "1st timeout must not trip");
        rc.note_poll_failed();
        assert!(!rc.note_transient_timeout(), "2nd timeout must not trip");

        // The third reaches MAX_CONSECUTIVE_TIMEOUTS → disconnect signal.
        rc.note_poll_failed();
        assert!(
            rc.note_transient_timeout(),
            "3rd consecutive timeout must trip the disconnect"
        );
    }

    /// A good poll mid-streak resets the sustained-timeout counter — the exact
    /// race the inline counter guards. timeout, timeout, good, timeout, timeout
    /// must NOT trip the disconnect.
    #[test]
    fn good_poll_resets_sustained_timeout_streak() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);
        rc.note_session_start();

        rc.note_poll_failed();
        assert!(!rc.note_transient_timeout()); // 1
        rc.note_poll_failed();
        assert!(!rc.note_transient_timeout()); // 2
                                               // A good poll breaks the streak.
        rc.note_good_poll(t0 + Duration::from_secs(1));
        // Now two more timeouts — still only 2 in a row, must not trip.
        rc.note_poll_failed();
        assert!(!rc.note_transient_timeout()); // 1 again
        rc.note_poll_failed();
        assert!(
            !rc.note_transient_timeout(),
            "streak reset by good poll — two timeouts after it must not trip"
        );
    }

    /// Dead-session back-off escalates across consecutive dead (zombie)
    /// sessions and resets the moment a session delivers a good poll.
    #[test]
    fn dead_session_backoff_escalates_then_resets_on_productive_session() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);

        // First zombie session: connect, no good poll, disconnect dead.
        rc.note_session_start();
        rc.note_session_end(); // consecutive_dead_sessions = 1
        let after_one = rc.reconnect_delay(t0); // dead_session_backoff(1) = 5s
        assert_eq!(after_one, Duration::from_secs(5));

        // Second zombie session → escalation.
        rc.note_session_start();
        rc.note_session_end(); // = 2
        let after_two = rc.reconnect_delay(t0); // dead_session_backoff(2) = 10s
        assert_eq!(after_two, Duration::from_secs(10));

        // A productive session resets the escalation.
        rc.note_session_start();
        rc.note_good_poll(t0); // marks the session productive
        rc.note_session_end(); // had_good_read → reset to 0
        let after_good = rc.reconnect_delay(t0); // dead_session_backoff(0) = 5s
        assert_eq!(
            after_good,
            Duration::from_secs(5),
            "productive session must reset the dead-session escalation"
        );
    }

    /// The flap gate engages on data starvation, then stands down only after a
    /// *sustained* run of good polls (not a single isolated success). This is
    /// the sticky-stand-down contract no pure-helper test could exercise.
    #[test]
    fn flap_engages_on_starvation_then_requires_sustained_recovery() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);

        // Starve past the threshold → flap engages on the next delay compute.
        let starved = t0 + Duration::from_secs(FLAP_THRESHOLD_SECS + 1);
        let elevated = rc.reconnect_delay(starved);
        assert_eq!(elevated, FLAP_ELEVATED_DELAY);
        assert!(rc.is_flap_engaged());

        // A single good poll does NOT stand down (needs FLAP_STANDDOWN_POLLS).
        rc.note_good_poll(starved + Duration::from_secs(1));
        assert!(
            rc.is_flap_engaged(),
            "one good poll must not stand the flap down"
        );

        // Two more good polls complete the stand-down (total = STANDDOWN_POLLS).
        rc.note_good_poll(starved + Duration::from_secs(2));
        assert!(
            rc.is_flap_engaged(),
            "two good polls is still short of stand-down"
        );
        rc.note_good_poll(starved + Duration::from_secs(3));
        assert!(
            !rc.is_flap_engaged(),
            "FLAP_STANDDOWN_POLLS consecutive good polls must stand the flap down"
        );
    }

    /// A failed poll aborts a partial flap stand-down — the recovery streak
    /// must be unbroken to stand down, matching the inline `consecutive_good_polls = 0`
    /// branch on poll failure.
    #[test]
    fn flap_standdown_aborted_by_failed_poll() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);
        rc.reconnect_delay(t0 + Duration::from_secs(FLAP_THRESHOLD_SECS + 1));
        assert!(rc.is_flap_engaged());

        // Two good polls toward the stand-down…
        rc.note_good_poll(t0 + Duration::from_secs(1));
        rc.note_good_poll(t0 + Duration::from_secs(2));
        // …then a failed poll wipes the streak.
        rc.note_poll_failed();
        // Two more good polls is still not enough (streak restarted from 0).
        rc.note_good_poll(t0 + Duration::from_secs(3));
        rc.note_good_poll(t0 + Duration::from_secs(4));
        assert!(
            rc.is_flap_engaged(),
            "a failed poll must restart the stand-down streak"
        );
        // A third unbroken good poll finally stands it down.
        rc.note_good_poll(t0 + Duration::from_secs(5));
        assert!(!rc.is_flap_engaged());
    }

    /// A manual `POST /api/reconnect` collapses every gate — flap, dead-session
    /// escalation, and connect back-off — back to the fast-retry state, and
    /// restarts the data-starvation clock so we don't immediately re-engage.
    #[test]
    fn manual_reconnect_resets_all_gates() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);

        // Park the controller in an engaged flap + escalated dead-session state.
        rc.reconnect_delay(t0 + Duration::from_secs(FLAP_THRESHOLD_SECS + 1)); // flap engaged
        rc.note_session_start();
        rc.note_session_end(); // dead session = 1
        assert!(rc.is_flap_engaged());

        // User clicks Reconnect → counter bumps 0 → 1.
        let reset_at = t0 + Duration::from_secs(1);
        assert!(rc.check_manual_reconnect(1, reset_at));
        assert!(!rc.is_flap_engaged(), "flap must clear on manual reconnect");

        // Delay collapses to the floor with no re-engage (clock restarted).
        let delay = rc.reconnect_delay(reset_at);
        assert_eq!(
            delay,
            Duration::from_secs(5),
            "all gates must collapse to the floor on manual reconnect"
        );

        // Same counter value → no-op.
        assert!(
            !rc.check_manual_reconnect(1, reset_at + Duration::from_secs(1)),
            "unchanged counter must not re-trigger a reset"
        );
    }

    /// The flap elevated delay dominates the dead-session cap and the connect
    /// back-off floor — the realistic "flapping dongle that connects but
    /// starves" combination, which is what the `Duration::max` selection was
    /// designed to resolve.
    #[test]
    fn flap_delay_dominates_dead_session_and_backoff_floor() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);

        // A flapping dongle: connects (note_session_start resets backoff to 5s),
        // disconnects dead a couple of times (dead_session_backoff → 10s), then
        // starves past the flap threshold.
        rc.note_session_start();
        rc.note_session_end();
        rc.note_session_start();
        rc.note_session_end(); // dead_sessions = 2 → 10s

        let delay = rc.reconnect_delay(t0 + Duration::from_secs(FLAP_THRESHOLD_SECS + 1));
        assert_eq!(
            delay, FLAP_ELEVATED_DELAY,
            "flap (30s) must dominate dead-session cap (10s) and backoff floor (5s)"
        );
    }

    /// On the connect-failure path (no session), the exponential back-off
    /// doubles each cycle and caps at 60 s, dominating the other gates.
    #[test]
    fn connect_backoff_escalates_and_caps_at_60s() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);

        // No note_session_start → connect() keeps failing → backoff escalates.
        rc.escalate_connect_backoff();
        assert_eq!(rc.reconnect_delay(t0), Duration::from_secs(10)); // 5 → 10
        rc.escalate_connect_backoff();
        assert_eq!(rc.reconnect_delay(t0), Duration::from_secs(20)); // 10 → 20
        rc.escalate_connect_backoff();
        rc.escalate_connect_backoff(); // 20 → 40 → 60
        assert_eq!(
            rc.reconnect_delay(t0),
            Duration::from_secs(60),
            "back-off must cap at 60s"
        );
        // Stays capped.
        rc.escalate_connect_backoff();
        assert_eq!(rc.reconnect_delay(t0), Duration::from_secs(60));
    }

    /// `note_session_start` resets the connect back-off — a successful connect
    /// means the network path is good, so the exponential must not carry over.
    #[test]
    fn successful_connect_resets_backoff_to_floor() {
        let t0 = now();
        let mut rc = ReconnectController::new(t0, 0);
        rc.escalate_connect_backoff();
        rc.escalate_connect_backoff(); // backoff = 20s
        rc.note_session_start(); // connect() succeeded
        assert_eq!(
            rc.reconnect_delay(t0),
            Duration::from_secs(5),
            "successful connect must reset the exponential back-off"
        );
    }
}

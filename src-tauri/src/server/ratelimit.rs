//! Bounded fixed-window rate limiting for the authenticated external API.
//!
//! Two limiter families protect the separate listener (never the main
//! dashboard server, whose traffic patterns are intentionally untouched):
//!
//! * per-source limits — keyed by the peer (or trusted-proxy-forwarded)
//!   address, applied to failed authentications and to reads;
//! * per-identity limits — keyed by the credential fingerprint, applied to
//!   control starts and stops.
//!
//! Memory is bounded: the map never grows beyond `max_entries`. When the cap
//! is reached, expired windows are evicted first and then the oldest windows
//! are dropped, so an attacker rotating spoofed addresses cannot grow the
//! map without bound.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Outcome of a rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitDecision {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Seconds until the current window resets (for `Retry-After`).
    pub retry_after_secs: u64,
}

#[derive(Debug)]
struct Window {
    count: u32,
    window_start: Instant,
}

/// A bounded fixed-window counter map. Clone-free, allocation-light, and
/// safe to share across tasks via an external mutex.
#[derive(Debug)]
pub struct RateLimiter<K: Hash + Eq + Clone> {
    limit: u32,
    window: Duration,
    max_entries: usize,
    windows: HashMap<K, Window>,
}

impl<K: Hash + Eq + Clone> RateLimiter<K> {
    /// Create a limiter allowing `limit` events per `window`, holding at
    /// most `max_entries` distinct keys.
    pub fn new(limit: u32, window: Duration, max_entries: usize) -> Self {
        Self {
            limit,
            window,
            max_entries,
            windows: HashMap::new(),
        }
    }

    /// Record one event for `key` and report whether it is within budget.
    pub fn check(&mut self, key: K) -> LimitDecision {
        self.check_n(key, 1)
    }

    /// Record `n` events for `key` (used to charge failed auths more
    /// aggressively than successful ones).
    pub fn check_n(&mut self, key: K, n: u32) -> LimitDecision {
        let now = Instant::now();
        let limit = self.limit;
        let window = self.window;
        let entry = self.windows.entry(key).or_insert(Window {
            count: 0,
            window_start: now,
        });
        if now.duration_since(entry.window_start) >= window {
            entry.count = 0;
            entry.window_start = now;
        }
        let retry_after_secs = window
            .saturating_sub(now.duration_since(entry.window_start))
            .as_secs()
            .max(1);
        if entry.count >= limit {
            return LimitDecision {
                allowed: false,
                retry_after_secs,
            };
        }
        entry.count = entry.count.saturating_add(n);
        // Bound memory after a successful insert.
        if self.windows.len() > self.max_entries {
            self.evict(now);
        }
        LimitDecision {
            allowed: true,
            retry_after_secs,
        }
    }

    /// Forget all accumulated windows (test helper for suites that exercise
    /// repeated actions through the same identity).
    pub fn clear(&mut self) {
        self.windows.clear();
    }

    /// Whether `key` is currently over its limit *without* recording an
    /// event (pre-check for failed-auth lockouts).
    pub fn is_limited(&self, key: &K) -> LimitDecision {
        let now = Instant::now();
        match self.windows.get(key) {
            Some(entry) if now.duration_since(entry.window_start) < self.window => {
                let retry_after_secs = self
                    .window
                    .saturating_sub(now.duration_since(entry.window_start))
                    .as_secs()
                    .max(1);
                LimitDecision {
                    allowed: entry.count < self.limit,
                    retry_after_secs,
                }
            }
            _ => LimitDecision {
                allowed: true,
                retry_after_secs: 1,
            },
        }
    }

    /// Evict expired windows, then the oldest windows, until under the cap.
    fn evict(&mut self, now: Instant) {
        self.windows
            .retain(|_, entry| now.duration_since(entry.window_start) < self.window);
        while self.windows.len() > self.max_entries {
            let oldest = self
                .windows
                .iter()
                .min_by_key(|(_, entry)| entry.window_start)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.windows.remove(&key);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit_then_blocks_until_window_resets() {
        let mut limiter: RateLimiter<u32> = RateLimiter::new(3, Duration::from_secs(60), 128);
        for _ in 0..3 {
            assert!(limiter.check(1).allowed);
        }
        let decision = limiter.check(1);
        assert!(!decision.allowed);
        assert!(decision.retry_after_secs >= 1 && decision.retry_after_secs <= 60);
    }

    #[test]
    fn windows_are_independent_per_key() {
        let mut limiter: RateLimiter<u32> = RateLimiter::new(1, Duration::from_secs(60), 128);
        assert!(limiter.check(1).allowed);
        assert!(!limiter.check(1).allowed);
        assert!(
            limiter.check(2).allowed,
            "a different key has its own budget"
        );
    }

    #[test]
    fn is_limited_does_not_consume_budget() {
        let mut limiter: RateLimiter<u32> = RateLimiter::new(1, Duration::from_secs(60), 128);
        assert!(limiter.check(7).allowed);
        assert!(!limiter.is_limited(&7).allowed);
        assert!(
            !limiter.check(7).allowed,
            "pre-check must not reset the window"
        );
    }

    #[test]
    fn map_is_bounded_under_key_flood() {
        let mut limiter: RateLimiter<u32> = RateLimiter::new(1000, Duration::from_secs(60), 64);
        for key in 0..10_000u32 {
            limiter.check(key);
        }
        assert!(
            limiter.windows.len() <= 64,
            "map len {} must stay bounded",
            limiter.windows.len()
        );
    }
}

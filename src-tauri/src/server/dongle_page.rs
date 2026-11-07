//! `GET /api/dongle-page` (issue #452): which of the WiFi dongle's own web
//! pages answers on the LAN, so the connection-problem banner can offer to
//! restart just the dongle from there instead of an isolator power-cycle.
//!
//! The probe only ever targets the configured inverter host (never anything
//! from the request), follows no redirects and ignores proxy settings, so it
//! can't be turned into a probe of other addresses. It sends no credentials
//! and changes nothing on the dongle: a page, a redirect or the login prompt
//! (401/403) counts as "the page is there"; a 404 or a server error doesn't.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::inverter::poll::AppState;

/// How long each probe may take, connect and reply together.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long an answer is reused. The dongle has a small connection table, and
/// this endpoint is open to the LAN, so repeated calls (several tabs, Retry
/// Now, a loop) must not each open sockets to it.
pub const CACHE_TTL: Duration = Duration::from_secs(5);

/// The last probe's answer for one host, so calls within [`CACHE_TTL`] share it.
#[derive(Default)]
pub struct ProbeCache {
    last: Option<(String, Instant, Option<String>)>,
}

impl ProbeCache {
    /// The answer for `host` if one was stored less than `ttl` before `now`.
    /// (`None` = nothing fresh; `Some(None)` = fresh "no page answered".)
    pub fn fresh(&self, host: &str, now: Instant, ttl: Duration) -> Option<Option<String>> {
        let (cached_host, at, url) = self.last.as_ref()?;
        // A clock that went backwards (`now` before `at`) counts as stale.
        let age = now.checked_duration_since(*at)?;
        (cached_host == host && age < ttl).then(|| url.clone())
    }

    pub fn store(&mut self, host: &str, now: Instant, url: Option<String>) {
        self.last = Some((host.to_string(), now, url));
    }
}

/// Held across the probe, so calls that arrive while one is running wait for
/// it and then read its answer instead of probing again.
static CACHE: tokio::sync::Mutex<ProbeCache> =
    tokio::sync::Mutex::const_new(ProbeCache { last: None });

/// The pages to try for `host`, in order of preference: the older WiFi
/// dongle's `/home.html` and the newer AECC dongle's `/web/info` (a
/// self-signed HTTPS page). Empty when the host isn't a plain IP address or
/// hostname, so nothing but the inverter's own address is ever probed.
pub fn candidates_for_host(host: &str) -> Vec<String> {
    let host = host.trim();
    // A pasted "ip:8899" still names the same machine; the dongle's page is
    // on the standard ports.
    let host = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !plain {
        return Vec::new();
    }
    vec![
        format!("http://{host}/home.html"),
        format!("https://{host}/web/info"),
    ]
}

/// Whether `url` answers within `timeout` with a page, a redirect or a login
/// prompt (401/403). Other errors (404, 5xx) mean this isn't the dongle's page.
fn answers(url: &str, timeout: Duration) -> bool {
    // Certificate verification is off for this probe only: the newer
    // dongle's page is self-signed, and nothing is sent or trusted.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .max_idle_connections(0)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .disable_verification(true)
                .build(),
        )
        .build();
    match ureq::Agent::new_with_config(config).get(url).call() {
        Ok(response) => {
            let status = response.status().as_u16();
            status < 400 || status == 401 || status == 403
        }
        Err(_) => false,
    }
}

/// The first of `candidates` that answers within `timeout`, probing them all
/// at once so the call is bounded by `timeout`. Blocking: call from
/// `spawn_blocking`.
pub fn find_dongle_page(candidates: &[String], timeout: Duration) -> Option<String> {
    let answered: Vec<bool> = std::thread::scope(|scope| {
        let probes: Vec<_> = candidates
            .iter()
            .map(|url| scope.spawn(move || answers(url, timeout)))
            .collect();
        probes
            .into_iter()
            .map(|probe| probe.join().unwrap_or(false))
            .collect()
    });
    candidates
        .iter()
        .zip(answered)
        .find_map(|(url, answered)| answered.then(|| url.clone()))
}

/// `GET /api/dongle-page` → `{ "ok": true, "url": "http://…" | null }`.
///
/// The host is the one the poll loop is using (the live settings), never
/// anything from the request. A page that doesn't answer is a result
/// (`null`), not an error.
pub async fn get_dongle_page(State(state): State<Arc<AppState>>) -> Json<Value> {
    // Copy the host out so the settings lock isn't held while probing.
    let host = state.settings.lock().await.host.clone();
    let candidates = candidates_for_host(&host);
    if candidates.is_empty() {
        return Json(json!({ "ok": true, "url": null }));
    }
    let mut cache = CACHE.lock().await;
    if let Some(url) = cache.fresh(&host, Instant::now(), CACHE_TTL) {
        return Json(json!({ "ok": true, "url": url }));
    }
    // No `AppState` lock is held while the probes wait.
    let url =
        match tokio::task::spawn_blocking(move || find_dongle_page(&candidates, PROBE_TIMEOUT))
            .await
        {
            Ok(url) => {
                cache.store(&host, Instant::now(), url.clone());
                url
            }
            // A failed probe task isn't an answer about the dongle: don't keep it.
            Err(_) => None,
        };
    Json(json!({ "ok": true, "url": url }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    const SHORT: Duration = Duration::from_millis(400);

    /// A local server that answers every request with `response`, counting
    /// the connections it gets. Returns the base URL and the counter.
    fn serve(response: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/page"), hits)
    }

    /// A local server that accepts connections and never answers.
    fn hang() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                held.push(stream);
            }
        });
        format!("http://127.0.0.1:{port}/page")
    }

    /// A URL nothing is listening on.
    fn closed() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}/page")
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
    const LOGIN: &str =
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    #[test]
    fn a_stored_answer_is_reused_until_it_expires() {
        let t0 = Instant::now();
        let mut cache = ProbeCache::default();
        assert_eq!(cache.fresh("h", t0, CACHE_TTL), None, "empty cache");

        cache.store("h", t0, Some("http://h/home.html".into()));
        let inside = t0 + CACHE_TTL - Duration::from_millis(1);
        assert_eq!(
            cache.fresh("h", inside, CACHE_TTL),
            Some(Some("http://h/home.html".to_string()))
        );
        assert_eq!(cache.fresh("h", t0 + CACHE_TTL, CACHE_TTL), None, "expired");
    }

    #[test]
    fn a_no_page_answer_is_cached_too() {
        let t0 = Instant::now();
        let mut cache = ProbeCache::default();
        cache.store("h", t0, None);
        assert_eq!(
            cache.fresh("h", t0 + Duration::from_secs(1), CACHE_TTL),
            Some(None)
        );
    }

    #[test]
    fn an_answer_is_never_reused_for_another_host() {
        let t0 = Instant::now();
        let mut cache = ProbeCache::default();
        cache.store(
            "192.168.1.5",
            t0,
            Some("http://192.168.1.5/home.html".into()),
        );
        assert_eq!(cache.fresh("192.168.1.6", t0, CACHE_TTL), None);
    }

    #[test]
    fn a_newer_answer_replaces_the_old_one() {
        let t0 = Instant::now();
        let mut cache = ProbeCache::default();
        cache.store("h", t0, None);
        let later = t0 + Duration::from_secs(6);
        cache.store("h", later, Some("http://h/home.html".into()));
        assert_eq!(
            cache.fresh("h", later, CACHE_TTL),
            Some(Some("http://h/home.html".to_string()))
        );
    }

    #[test]
    fn candidates_are_the_two_dongle_pages_for_the_host() {
        assert_eq!(
            candidates_for_host("192.168.1.50"),
            vec![
                "http://192.168.1.50/home.html".to_string(),
                "https://192.168.1.50/web/info".to_string(),
            ]
        );
    }

    #[test]
    fn candidates_accept_a_hostname_and_trim_a_pasted_port() {
        assert_eq!(
            candidates_for_host(" inverter.local ")[0],
            "http://inverter.local/home.html"
        );
        assert_eq!(
            candidates_for_host("192.168.1.50:8899")[0],
            "http://192.168.1.50/home.html"
        );
    }

    #[test]
    fn candidates_refuse_anything_that_is_not_a_plain_host() {
        for bad in [
            "",
            "   ",
            "evil.example/path",
            "http://192.168.1.50",
            "192.168.1.50 extra",
            "user@192.168.1.50",
            "192.168.1.50?x=1",
            "192.168.1.50#frag",
            "[::1]",
            "192.168.1.50:notaport",
        ] {
            assert!(
                candidates_for_host(bad).is_empty(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn the_first_page_wins_when_both_answer() {
        let (first, _) = serve(OK);
        let (second, _) = serve(OK);
        assert_eq!(
            find_dongle_page(&[first.clone(), second], SHORT),
            Some(first)
        );
    }

    #[test]
    fn the_second_page_is_used_when_only_it_answers() {
        let (second, _) = serve(OK);
        assert_eq!(
            find_dongle_page(&[closed(), second.clone()], SHORT),
            Some(second)
        );
    }

    #[test]
    fn a_login_prompt_counts_as_the_page_being_there() {
        let (page, _) = serve(LOGIN);
        assert_eq!(
            find_dongle_page(std::slice::from_ref(&page), SHORT),
            Some(page)
        );
    }

    #[test]
    fn an_error_page_is_not_the_dongle_page() {
        // A dongle serving HTTP that has no /home.html (404) or is failing
        // (500) must not win over the page that really answers.
        let not_found: &'static str =
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let broken: &'static str =
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (missing, _) = serve(not_found);
        let (failing, _) = serve(broken);
        let (real, _) = serve(OK);
        assert_eq!(
            find_dongle_page(std::slice::from_ref(&missing), SHORT),
            None
        );
        assert_eq!(
            find_dongle_page(std::slice::from_ref(&failing), SHORT),
            None
        );
        assert_eq!(
            find_dongle_page(&[missing, failing, real.clone()], SHORT),
            Some(real)
        );
    }

    #[test]
    fn a_forbidden_page_still_counts_as_there() {
        let forbidden: &'static str =
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (page, _) = serve(forbidden);
        assert_eq!(
            find_dongle_page(std::slice::from_ref(&page), SHORT),
            Some(page)
        );
    }

    #[test]
    fn no_answer_gives_none() {
        assert_eq!(find_dongle_page(&[closed(), closed()], SHORT), None);
        assert_eq!(find_dongle_page(&[], SHORT), None);
    }

    #[test]
    fn a_page_that_hangs_cannot_hold_the_call_up_or_beat_one_that_answers() {
        let (second, _) = serve(OK);
        let started = Instant::now();
        let found = find_dongle_page(&[hang(), second.clone()], SHORT);
        assert_eq!(found, Some(second));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_redirect_counts_as_an_answer_and_is_never_followed() {
        let (elsewhere, elsewhere_hits) = serve(OK);
        let redirect: &'static str = Box::leak(
            format!("HTTP/1.1 302 Found\r\nLocation: {elsewhere}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .into_boxed_str(),
        );
        let (page, _) = serve(redirect);
        assert_eq!(
            find_dongle_page(std::slice::from_ref(&page), SHORT),
            Some(page)
        );
        assert_eq!(
            elsewhere_hits.load(Ordering::SeqCst),
            0,
            "the redirect target was contacted"
        );
    }
}

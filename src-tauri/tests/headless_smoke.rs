//! Non-destructive smoke test for `givenergy-local --headless`.
//!
//! `lib.rs` has 30+ lines of one-shot setup in `init_tracing` and
//! `run_headless` that fundamentally can't be exercised in-process:
//! `tracing::subscriber::init()` panics on a second call, so any
//! test that touches the global subscriber would poison the rest of
//! the test binary. The same is true of the bind-to-port startup in
//! `run_headless` — if a unit test spawned it, the test process
//! would either keep a port bound (and break the next test) or fail
//! to bind and report a confusing error.
//!
//! The pragmatic answer: spawn the real `givenergy-local` binary as
//! a subprocess on an ephemeral port, wait for the HTTP server, hit
//! a handful of endpoints to confirm init_tracing and run_headless
//! both ran to completion, then kill and reap. This is hermetic (no
//! external services, no shared state with the rest of the test
//! suite) and non-destructive (no production state, no leaked
//! ports).
//!
//! The test only runs when the binary is already built. It picks
//! `target/debug/givenergy-local` for `cargo test` runs and
//! `target/release/givenergy-local` for release-mode runs. If
//! neither exists the test is skipped with a printed reason — it
//! must never break a developer's `cargo test` run just because
//! they haven't built the binary.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct TempConfig {
    root: PathBuf,
    config: PathBuf,
    home: PathBuf,
}

impl TempConfig {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "givenergy-local-headless-smoke-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = root.join("config");
        let home = root.join("home");
        std::fs::create_dir_all(&config).expect("create isolated config directory");
        std::fs::create_dir_all(&home).expect("create isolated home directory");
        Self { root, config, home }
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Resolve the headless binary path. Returns `None` if neither
/// debug nor release build is present.
fn binary_path() -> Option<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let candidates = [
        PathBuf::from(manifest_dir).join("target/debug/givenergy-local"),
        PathBuf::from(manifest_dir).join("target/release/givenergy-local"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Bind a free port, hand it back as a u16, and immediately drop
/// the listener so the kernel can release it. The port is racy
/// (something else could grab it in the window between drop and
/// the binary's bind) but the binary will fail to start in that
/// case, the test will time out, and the next run will likely
/// succeed. We bind 127.0.0.1 only to avoid leaking the chosen
/// port to the network.
fn pick_ephemeral_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener
        .local_addr()
        .expect("local_addr on just-bound socket")
        .port();
    drop(listener);
    port
}

/// Poll `url` until it returns any HTTP response, or `timeout`
/// elapses. Any HTTP status counts as "the server is up" — the
/// endpoint may legitimately 4xx for our test inputs. We use a
/// raw TCP connect for the poll loop (faster than a full HTTP
/// round-trip and avoids ureq's default behaviour of blocking
/// for a long time on connection errors).
fn wait_for_http(url: &str, timeout: Duration) -> Result<(), String> {
    // Parse the host:port out of the URL. We only need the
    // authority — the path is irrelevant to "is the server
    // listening".
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|s| s.split('/').next())
        .ok_or_else(|| format!("could not parse authority from {url}"))?;
    let deadline = Instant::now() + timeout;
    let mut last_err = String::new();
    while Instant::now() < deadline {
        match std::net::TcpStream::connect_timeout(
            &authority.parse().map_err(|e| format!("bad address: {e}"))?,
            Duration::from_millis(500),
        ) {
            Ok(_stream) => return Ok(()),
            Err(e) => last_err = format!("{e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "server did not become ready within {timeout:?}: {last_err}"
    ))
}

/// Force-kill the subprocess and reap. We don't attempt a graceful
/// shutdown signal because (a) the SIGTERM-vs-SIGKILL dance is
/// platform-specific and (b) the test only cares that the binary
/// reaches a responsive HTTP server, not that it exits cleanly.
fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn headless_init_tracing_and_run_headless_reach_a_responsive_http_server() {
    // Locate the binary. Skip with a clear message if the project
    // hasn't been built — this must not fail the test suite on a
    // fresh checkout where `cargo test` is the first command.
    let Some(bin) = binary_path() else {
        eprintln!(
            "skipping headless smoke test: neither target/debug nor target/release \
             contains givenergy-local. Run `cargo build` (or `cargo test --no-run` \
             which builds it as a side effect) to enable this test."
        );
        return;
    };

    // Pick a port the test owns. We never let the binary pick its
    // own default (7337) because the E2E suite and the production
    // app both want that one.
    let port = pick_ephemeral_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let temp = TempConfig::new();

    // Spawn the binary. We deliberately do NOT pass --dist so the
    // headless startup exercises the API-only fallback path inside
    // resolve_dist_dir (i.e. resolve_dist_dir returns None, then
    // start_server is called instead of start_server_with_frontend).
    // That path is otherwise only covered by the e2e suite, and
    // exercising it here means a regression in resolve_dist_dir's
    // "no dist found" branch breaks this test before users see it.
    let mut child = Command::new(&bin)
        .args(["--headless", "--port", &port.to_string()])
        .env("GIVENERGY_LOCAL_CONFIG_DIR", &temp.config)
        .env("HOME", &temp.home)
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn givenergy-local --headless");

    // The init_tracing + bind-to-port path takes a few hundred ms
    // in CI; 10s is comfortably above the cold-start case.
    let ready = wait_for_http(&format!("{base_url}/api/status"), Duration::from_secs(10));
    let result = ready.and_then(|()| {
        // Hit a handful of endpoints to confirm both init paths ran
        // to completion: the in-memory LogRing is wired (we
        // exercise /api/logs), the connection_state machine is
        // alive (initial Disconnected state is what /api/status
        // reports before the first poll), and the EVC subsystem
        // didn't crash (its status endpoint is always available).
        for path in [
            "/api/status",
            "/api/logs",
            "/api/log-level",
            "/api/evc/status",
        ] {
            let resp = ureq::get(&format!("{base_url}{path}"))
                .call()
                .map_err(|e| format!("GET {path}: {e}"))?;
            // ureq 3.x exposes a `StatusCode` from `http` (not the
            // std one) so we compare against its constants rather
            // than raw integers.
            if resp.status().is_server_error() {
                return Err(format!("GET {path} returned 5xx: {}", resp.status()));
            }
        }
        Ok(())
    });

    // Always tear the process down before reporting — a leaked
    // binary would hold the port and break the next test run.
    terminate(&mut child);

    // Surface the child's stderr if anything went wrong, so a
    // failure points at the actual log line rather than "no
    // response".
    if result.is_err() {
        if let Some(mut stderr) = child.stderr.take() {
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf);
            eprintln!("--- child stderr ---\n{buf}\n--- end stderr ---");
        }
    }
    result.expect("headless startup reached a responsive HTTP server");
}

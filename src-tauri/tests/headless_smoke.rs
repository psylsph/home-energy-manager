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
use std::thread::JoinHandle;
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

/// Resolve the headless binary path for the profile running this test.
/// Cargo exposes the exact executable path to integration tests; use it when
/// available so custom target directories and executable naming are handled
/// correctly. The profile-relative fallback is needed when the binary was
/// built separately (for example by a release CI job).
fn binary_path() -> Option<PathBuf> {
    for variable in [
        "CARGO_BIN_EXE_givenergy-local",
        "CARGO_BIN_EXE_givenergy_local",
    ] {
        if let Some(path) = std::env::var_os(variable).map(PathBuf::from) {
            if path.is_file() {
                return Some(path);
            }
        }
    }

    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    binary_path_for_profile(
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        target_dir,
    )
}

fn binary_path_for_profile(
    profile: &str,
    manifest_dir: PathBuf,
    target_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    let target_dir = target_dir.unwrap_or_else(|| manifest_dir.join("target"));
    let binary = format!("givenergy-local{}", std::env::consts::EXE_SUFFIX);
    let candidate = target_dir.join(profile).join(binary);
    candidate.is_file().then_some(candidate)
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

/// Poll HEM's status endpoint until it returns the expected JSON identity or
/// `timeout` elapses. Each attempt has its own short connect/read deadline so
/// a foreign listener that accepts and then goes silent cannot hang teardown.
fn wait_for_hem(url: &str, timeout: Duration) -> Result<(), String> {
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|s| s.split('/').next())
        .ok_or_else(|| format!("could not parse authority from {url}"))?;
    let address = authority
        .parse::<std::net::SocketAddr>()
        .map_err(|e| format!("bad address: {e}"))?;
    let deadline = Instant::now() + timeout;
    let mut last_err = String::new();
    while Instant::now() < deadline {
        match request_hem_status(&address) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "server did not become ready within {timeout:?}: {last_err}"
    ))
}

fn request_hem_status(address: &std::net::SocketAddr) -> Result<(), String> {
    use std::io::Write;

    let attempt_timeout = Duration::from_millis(250);
    let mut stream = std::net::TcpStream::connect_timeout(address, attempt_timeout)
        .map_err(|e| format!("connect failed: {e}"))?;
    stream
        .set_read_timeout(Some(attempt_timeout))
        .map_err(|e| format!("set read timeout failed: {e}"))?;
    stream
        .set_write_timeout(Some(attempt_timeout))
        .map_err(|e| format!("set write timeout failed: {e}"))?;
    stream
        .write_all(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .map_err(|e| format!("write failed: {e}"))?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buffer[..n]);
                if response.len() > 128 * 1024 {
                    return Err("response exceeded 128 KiB".to_string());
                }
                if response_complete(&response) {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "response had no HTTP headers".to_string())?;
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|e| format!("response headers were not UTF-8: {e}"))?;
    let status_line = headers.lines().next().unwrap_or_default();
    if !(status_line.starts_with("HTTP/1.1 200 ") || status_line.starts_with("HTTP/1.0 200 ")) {
        return Err(format!("unexpected HTTP status: {status_line}"));
    }
    let body = &response[header_end + 4..];
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("status response was not JSON: {e}"))?;
    if json.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        || json.get("connection").is_none()
    {
        return Err("status response was not a HEM status payload".to_string());
    }
    Ok(())
}

fn response_complete(response: &[u8]) -> bool {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let Some(content_length) = headers.lines().find_map(|line| {
        line.strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
    }) else {
        return false;
    };
    response.len() >= header_end + 4 + content_length
}

/// Force-kill the subprocess and reap. We don't attempt a graceful
/// shutdown signal because (a) the SIGTERM-vs-SIGKILL dance is
/// platform-specific and (b) the test only cares that the binary
/// reaches a responsive HTTP server, not that it exits cleanly.
const MAX_CHILD_OUTPUT_BYTES: usize = 64 * 1024;

struct ChildOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct OutputDrainers {
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

fn drain_reader<R: Read>(mut reader: R) -> Vec<u8> {
    let mut captured = Vec::with_capacity(MAX_CHILD_OUTPUT_BYTES);
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let remaining = MAX_CHILD_OUTPUT_BYTES.saturating_sub(captured.len());
        if remaining > 0 {
            captured.extend_from_slice(&buffer[..bytes_read.min(remaining)]);
        }
    }
    captured
}

fn spawn_output_drainers(child: &mut Child) -> OutputDrainers {
    let stdout = child
        .stdout
        .take()
        .map(|reader| std::thread::spawn(move || drain_reader(reader)));
    let stderr = child
        .stderr
        .take()
        .map(|reader| std::thread::spawn(move || drain_reader(reader)));
    OutputDrainers { stdout, stderr }
}

fn join_output_drainer(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default()
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn terminate(child: &mut Child, drainers: OutputDrainers) -> ChildOutput {
    let _ = child.kill();
    let _ = child.wait();
    ChildOutput {
        stdout: join_output_drainer(drainers.stdout),
        stderr: join_output_drainer(drainers.stderr),
    }
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
    let drainers = spawn_output_drainers(&mut child);

    // The init_tracing + bind-to-port path takes a few hundred ms
    // in CI; 10s is comfortably above the cold-start case.
    let ready = wait_for_hem(&format!("{base_url}/api/status"), Duration::from_secs(10));
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
    let output = terminate(&mut child, drainers);

    // Surface the child's stderr if anything went wrong, so a
    // failure points at the actual log line rather than "no
    // response".
    if result.is_err() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("--- child stderr ---\n{stderr}\n--- end stderr ---");
    }
    result.expect("headless startup reached a responsive HTTP server");
}

#[test]
fn headless_exits_when_http_port_is_already_occupied() {
    let Some(bin) = binary_path() else {
        eprintln!("skipping headless launch-failure test: givenergy-local binary is not built");
        return;
    };

    let temp = TempConfig::new();
    let occupied = std::net::TcpListener::bind("0.0.0.0:0").expect("bind occupied test port");
    let port = occupied
        .local_addr()
        .expect("local_addr on occupied test port")
        .port();
    let mut child = Command::new(&bin)
        .args(["--headless", "--port", &port.to_string()])
        .env("GIVENERGY_LOCAL_CONFIG_DIR", &temp.config)
        .env("HOME", &temp.home)
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn givenergy-local --headless");
    let drainers = spawn_output_drainers(&mut child);

    let status = wait_for_exit(&mut child, Duration::from_secs(10));
    let output = terminate(&mut child, drainers);

    assert!(
        status.is_some(),
        "headless process remained alive after HTTP bind failure; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(occupied);
    std::net::TcpListener::bind(("0.0.0.0", port))
        .expect("headless launch failure must not leave the HTTP port occupied");
}

#[cfg(test)]
mod tests {
    use super::{binary_path_for_profile, spawn_output_drainers, terminate, wait_for_hem};
    use std::path::PathBuf;

    #[test]
    fn binary_path_uses_the_active_profile_not_the_other_profile() {
        let root = std::env::temp_dir().join(format!(
            "givenergy-local-profile-resolver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let debug = root.join("target/debug/givenergy-local");
        let release = root.join("target/release/givenergy-local");
        std::fs::create_dir_all(debug.parent().unwrap()).unwrap();
        std::fs::create_dir_all(release.parent().unwrap()).unwrap();
        std::fs::write(&debug, b"debug sentinel").unwrap();
        std::fs::write(&release, b"release sentinel").unwrap();

        assert_eq!(
            binary_path_for_profile("debug", PathBuf::from(&root), None),
            Some(debug.clone())
        );
        assert_eq!(
            binary_path_for_profile("release", PathBuf::from(&root), None),
            Some(release.clone())
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn chatty_child_output_does_not_block_reap_and_is_bounded() {
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2; sleep 30",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn chatty fixture");
        let drainers = spawn_output_drainers(&mut child);

        let started = std::time::Instant::now();
        let output = terminate(&mut child, drainers);

        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "chatty child teardown took too long"
        );
        assert!(output.stdout.len() <= super::MAX_CHILD_OUTPUT_BYTES);
        assert!(output.stderr.len() <= super::MAX_CHILD_OUTPUT_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn readiness_rejects_a_foreign_accept_only_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = std::thread::spawn(move || {
            let Ok((_stream, _address)) = listener.accept() else {
                return;
            };
            std::thread::sleep(std::time::Duration::from_millis(500));
        });

        let started = std::time::Instant::now();
        let result = wait_for_hem(
            &format!("http://127.0.0.1:{port}/api/status"),
            std::time::Duration::from_millis(250),
        );

        assert!(
            result.is_err(),
            "a foreign listener must not count as ready"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "foreign readiness probe must have a bounded deadline"
        );
        acceptor.join().unwrap();
    }
}

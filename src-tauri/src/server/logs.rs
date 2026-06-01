//! Log capture for the developer console.
//!
//! Provides a ring buffer that stores recent log lines and a
//! `tracing` subscriber layer that captures formatted output.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use parking_lot::Mutex;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Ring buffer
// ---------------------------------------------------------------------------

/// A fixed-capacity ring buffer of log lines.
///
/// Thread-safe via `parking_lot::Mutex`. Old entries are
/// evicted when the buffer is full.
///
/// Also holds a runtime-adjustable minimum log level that controls
/// what severity events are actually captured into the buffer.
/// The frontend developer console can change this via the API.
pub struct LogRing {
    buf: Mutex<LogRingInner>,
    /// Minimum log level to capture: 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
    pub min_level: AtomicU8,
}

struct LogRingInner {
    data: Vec<String>,
    capacity: usize,
    cursor: usize, // next write position
    len: usize,    // number of valid entries
}

impl LogRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: Mutex::new(LogRingInner {
                data: vec![String::new(); capacity],
                capacity,
                cursor: 0,
                len: 0,
            }),
            min_level: AtomicU8::new(2), // default: INFO
        }
    }

    /// Push a log line into the ring buffer.
    pub fn push(&self, line: &str) {
        let mut inner = self.buf.lock();
        let cursor = inner.cursor;
        inner.data[cursor] = line.to_string();
        inner.cursor = (cursor + 1) % inner.capacity;
        if inner.len < inner.capacity {
            inner.len += 1;
        }
    }

    /// Read all stored lines in chronological order.
    pub fn read_all(&self) -> Vec<String> {
        let inner = self.buf.lock();
        if inner.len == 0 {
            return Vec::new();
        }
        // The oldest entry is at `cursor` when the buffer is full,
        // or at index 0 when partially filled.
        let start = if inner.len < inner.capacity {
            0
        } else {
            inner.cursor
        };
        let mut result = Vec::with_capacity(inner.len);
        for i in 0..inner.len {
            let idx = (start + i) % inner.capacity;
            result.push(inner.data[idx].clone());
        }
        result
    }
}

// ---------------------------------------------------------------------------
// REST endpoint
// ---------------------------------------------------------------------------

/// GET /api/logs — return recent log lines.
///
/// Query params: `?after=<n>` returns lines starting from index `n`.
pub async fn get_logs(State(state): State<Arc<crate::inverter::poll::AppState>>) -> Json<Value> {
    let lines = state.log_ring.read_all();
    Json(json!({
        "ok": true,
        "lines": lines,
        "count": lines.len(),
    }))
}

/// Convert a level string to the atomic u8 value used by `LogRing::min_level`.
fn level_str_to_u8(level: &str) -> Option<u8> {
    match level {
        "ERROR" => Some(0),
        "WARN" => Some(1),
        "INFO" => Some(2),
        "DEBUG" => Some(3),
        "TRACE" => Some(4),
        _ => None,
    }
}

/// Convert a u8 level value back to its string representation.
fn level_u8_to_str(level: u8) -> &'static str {
    match level {
        0 => "ERROR",
        1 => "WARN",
        2 => "INFO",
        3 => "DEBUG",
        4 => "TRACE",
        _ => "INFO",
    }
}

/// GET /api/log-level — return current capture level.
pub async fn get_log_level(State(state): State<Arc<crate::inverter::poll::AppState>>) -> Json<Value> {
    let level = state.log_ring.min_level.load(std::sync::atomic::Ordering::Relaxed);
    Json(json!({
        "ok": true,
        "level": level_u8_to_str(level),
        "level_code": level,
    }))
}

/// PUT /api/log-level — set the capture level.
///
/// Body: `{ "level": "DEBUG" }` — one of ERROR, WARN, INFO, DEBUG, TRACE.
pub async fn set_log_level(
    State(state): State<Arc<crate::inverter::poll::AppState>>,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> Json<Value> {
    let level_name = body.get("level").and_then(|v| v.as_str()).unwrap_or("");
    match level_str_to_u8(level_name) {
        Some(level_code) => {
            state.log_ring.min_level.store(level_code, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(%level_name, "Log capture level changed");
            Json(json!({
                "ok": true,
                "level": level_name,
                "level_code": level_code,
            }))
        }
        None => Json(json!({
            "ok": false,
            "error": format!("Invalid level: {level_name:?}. Use ERROR, WARN, INFO, DEBUG, or TRACE."),
        })),
    }
}

// ---------------------------------------------------------------------------
// Tracing layer for log capture
// ---------------------------------------------------------------------------

use std::fmt::Write;
use tracing::Subscriber;
use tracing_subscriber::Layer;

/// A `tracing-subscriber` layer that captures formatted log events
/// into a `LogRing`.
pub struct LogCaptureLayer {
    ring: Arc<LogRing>,
}

impl LogCaptureLayer {
    pub fn new(ring: Arc<LogRing>) -> Self {
        Self { ring }
    }
}

impl<S> Layer<S> for LogCaptureLayer
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Check if this event meets the minimum capture level.
        let event_level = event.metadata().level();
        let event_code = if *event_level <= tracing::Level::ERROR {
            0
        } else if *event_level <= tracing::Level::WARN {
            1
        } else if *event_level <= tracing::Level::INFO {
            2
        } else if *event_level <= tracing::Level::DEBUG {
            3
        } else {
            4
        };
        if event_code > self.ring.min_level.load(Ordering::Relaxed) {
            return; // below minimum capture level — skip
        }

        let mut buf = String::new();
        let now = chrono::Local::now();
        write!(buf, "{} ", now.format("%H:%M:%S%.3f")).ok();

        // Level
        let level_str = if *event_level <= tracing::Level::ERROR {
            "ERROR"
        } else if *event_level <= tracing::Level::WARN {
            "WARN"
        } else if *event_level <= tracing::Level::INFO {
            "INFO"
        } else if *event_level <= tracing::Level::DEBUG {
            "DEBUG"
        } else {
            "TRACE"
        };
        write!(buf, "{level_str} ").ok();

        // Target/module path
        let target = event.metadata().target();
        let short = target.split("::").next().unwrap_or(target);
        write!(buf, "[{short}] ").ok();

        // Fields
        let mut visitor = FieldVisitor { buf: &mut buf };
        event.record(&mut visitor);

        self.ring.push(&buf);
    }
}

struct FieldVisitor<'a> {
    buf: &'a mut String,
}

impl<'a> tracing::field::Visit for FieldVisitor<'a> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.buf.push_str(value);
        } else {
            write!(self.buf, " {}={}", field.name(), value).ok();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            write!(self.buf, "{:?}", value).ok();
        } else {
            write!(self.buf, " {}={:?}", field.name(), value).ok();
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        write!(self.buf, " {}={}", field.name(), value).ok();
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        write!(self.buf, " {}={}", field.name(), value).ok();
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        write!(self.buf, " {}={}", field.name(), value).ok();
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        write!(self.buf, " {}={}", field.name(), value).ok();
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        write!(self.buf, " {}={}", field.name(), value).ok();
    }
}

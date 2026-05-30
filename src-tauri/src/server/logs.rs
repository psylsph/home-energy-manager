//! Log capture for the developer console.
//!
//! Provides a ring buffer that stores recent log lines and a
//! `tracing` subscriber layer that captures formatted output.

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
pub struct LogRing {
    buf: Mutex<LogRingInner>,
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
        let mut buf = String::new();
        let now = chrono::Local::now();
        write!(buf, "{} ", now.format("%H:%M:%S%.3f")).ok();

        // Level
        let level = if event.metadata().level() <= &tracing::Level::ERROR {
            "ERROR"
        } else if event.metadata().level() <= &tracing::Level::WARN {
            "WARN"
        } else if event.metadata().level() <= &tracing::Level::INFO {
            "INFO"
        } else if event.metadata().level() <= &tracing::Level::DEBUG {
            "DEBUG"
        } else {
            "TRACE"
        };
        write!(buf, "{level} ").ok();

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

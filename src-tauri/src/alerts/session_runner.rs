//! The I/O shared by the alerts built on a persisted per-poll state machine:
//! "Car charging started / finished" (issue #454) and "Export finished"
//! (issue #499). Each poll the runner loads the saved session once, feeds it
//! the poll, saves it when it changes shape or a minute has passed, sends
//! what ended, and keeps an undelivered message for a resend. Switched off,
//! it drops the session, the held messages and the saved copy.

use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

/// How often the session is saved when nothing else changed.
const PERSIST_EVERY_SECS: i64 = 60;

/// A state machine the runner can keep.
pub(crate) trait AlertSession:
    Default + Clone + PartialEq + Serialize + DeserializeOwned + Send + 'static
{
    /// `meta` key holding the saved session.
    const META_KEY: &'static str;
    /// Named in the log when a message isn't delivered.
    const WHAT: &'static str;
    /// What a change of is worth saving at once (a session opening or
    /// closing, a run starting or ending); other changes wait for the minute.
    type Shape: PartialEq;
    fn shape(&self) -> Self::Shape;
}

/// The session plus what the runner keeps around it.
#[derive(Debug, Default)]
pub struct SessionRunner<S> {
    loaded: bool,
    pub(crate) session: S,
    persisted_at: Option<i64>,
    outbox: crate::alerts::battery_health::SocOutbox,
}

/// One poll. `step` runs only while `on`, and returns whether anything
/// ended plus the messages to send (an ended session may send none).
pub(crate) async fn run_session<S, F, Fut>(
    runner: &Arc<Mutex<SessionRunner<S>>>,
    db: Option<Arc<crate::history::HistoryDb>>,
    config: crate::settings::AlertsConfig,
    on: bool,
    now_ts: i64,
    step: impl FnOnce(&mut S) -> (bool, Vec<String>),
    send: F,
) -> Vec<tokio::task::JoinHandle<()>>
where
    S: AlertSession,
    F: Fn(crate::settings::AlertsConfig, String) -> Fut,
    Fut: std::future::Future<Output = bool> + Send + 'static,
{
    let (messages, resend, to_save) = {
        let mut guard = runner.lock().await;
        if on && !guard.loaded && db.is_none() {
            // The saved session can't be read yet: wait for the database
            // rather than start a new one over it and announce twice.
            return Vec::new();
        }
        if !guard.loaded {
            guard.loaded = true;
            if let Some(saved) = db
                .as_ref()
                .and_then(|db| db.get_meta_value(S::META_KEY))
                .and_then(|json| serde_json::from_str::<S>(&json).ok())
            {
                guard.session = saved;
            }
        }
        if !on {
            let had_state = guard.session != S::default() || guard.persisted_at.is_some();
            guard.session = S::default();
            guard.outbox = Default::default();
            guard.persisted_at = None;
            let clear = had_state
                .then(|| serde_json::to_string(&S::default()).ok())
                .flatten();
            drop(guard);
            if let (Some(db), Some(json)) = (db, clear) {
                let _ = tokio::task::spawn_blocking(move || db.set_meta_value(S::META_KEY, &json))
                    .await;
            }
            return Vec::new();
        }
        let before = guard.session.shape();
        let (ended, messages) = step(&mut guard.session);
        let resend = guard.outbox.take_due(now_ts);
        let due = guard
            .persisted_at
            .is_none_or(|at| now_ts < at || now_ts - at >= PERSIST_EVERY_SECS);
        let save = ended || before != guard.session.shape() || due;
        let to_save = save.then(|| {
            guard.persisted_at = Some(now_ts);
            serde_json::to_string(&guard.session).ok()
        });
        (messages, resend, to_save.flatten())
    };

    if let (Some(db), Some(json)) = (db, to_save) {
        let _ = tokio::task::spawn_blocking(move || db.set_meta_value(S::META_KEY, &json)).await;
    }

    // Each message goes out on its own task; one that isn't delivered is held
    // for a resend, carrying when it first failed.
    let mut handles = Vec::new();
    let mut deliver = |texts: Vec<(i64, String)>| {
        let text = texts
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let delivery = send(config.clone(), text);
        let runner = runner.clone();
        handles.push(tokio::spawn(async move {
            if !delivery.await {
                tracing::warn!("{} message not delivered, will resend", S::WHAT);
                runner.lock().await.outbox.add_failed(texts, now_ts);
            }
        }));
    };
    if !resend.is_empty() {
        deliver(resend);
    }
    for message in messages {
        deliver(vec![(now_ts, message)]);
    }
    handles
}

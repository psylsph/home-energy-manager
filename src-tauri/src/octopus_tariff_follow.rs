//! Following the rates Octopus charges in Settings → Energy Tariffs
//! (issue #455): the preview of what could be followed, the switch between
//! Manual and Octopus, and the refresh after each rates sync.
//!
//! The derivation itself is pure ([`crate::octopus_tariff`]). This is the part
//! that touches the history database and the settings file. A failed or
//! partial sync, or rates that can't be written as fixed windows, never
//! changes the saved tariff: a part that can't be derived is left exactly as
//! it was.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::history::HistoryDb;
use crate::inverter::poll::AppState;
use crate::octopus_tariff::{
    apply_to_settings, derive_all, gas_unit_rate_usable, Derivations, DerivedGas, DerivedTariff,
    NotDerivable,
};
use crate::settings::{Settings, TariffSource};

async fn derive(db: Arc<HistoryDb>, settings: Settings, now: i64) -> Result<Derivations, String> {
    tokio::task::spawn_blocking(move || derive_all(&db, &settings, now))
        .await
        .map_err(|e| format!("Tariff derivation task failed: {e}"))?
}

/// Write `derived` into the saved settings, but only while the source is still
/// Octopus: the user may have switched to Manual since the rates were read, and
/// that choice wins. Returns whether the file changed.
pub async fn apply_if_following(derived: Derivations, now: i64) -> Result<bool, String> {
    Settings::update_async(move |s| {
        s.tariff_source == TariffSource::Octopus && apply_to_settings(s, &derived, now)
    })
    .await
}

/// After a rates sync: if Energy Tariffs follow Octopus, bring them up to date.
pub async fn follow_after_sync(db: Arc<HistoryDb>, now: i64) -> Result<(), String> {
    let settings = Settings::load_async().await;
    if settings.tariff_source != TariffSource::Octopus {
        return Ok(());
    }
    let derived = derive(db, settings, now).await?;
    apply_if_following(derived, now).await?;
    Ok(())
}

/// [`follow_after_sync`], but only when the tariff download had no errors: a
/// sync that failed part-way may have left rates missing, and a failed or
/// partial sync must never change the saved tariff.
pub async fn follow_after_clean_sync(
    db: Arc<HistoryDb>,
    now: i64,
    tariff_error: Option<&str>,
) -> Result<(), String> {
    if tariff_error.is_some() {
        return Ok(());
    }
    follow_after_sync(db, now).await
}

fn tariff_json(result: &Result<DerivedTariff, NotDerivable>) -> Value {
    match result {
        Ok(t) => json!({
            "ok": true,
            "tariff_code": t.tariff_code,
            "slots": t.config.slots,
            "standing_charge_p_per_day": t.standing_charge_p_per_day,
        }),
        Err(e) => json!({"ok": false, "reason": e.reason()}),
    }
}

fn gas_json(result: &Result<DerivedGas, NotDerivable>, unit_rate_used: bool) -> Value {
    match result {
        Ok(g) => json!({
            "ok": true,
            "tariff_code": g.tariff_code,
            "unit_rate_p_per_kwh": g.unit_rate_p_per_kwh,
            "standing_charge_p_per_day": g.standing_charge_p_per_day,
            "unit_rate_used": unit_rate_used,
        }),
        Err(e) => json!({"ok": false, "reason": e.reason()}),
    }
}

/// What could be followed, and the current source and what it last filled.
fn status_json(settings: &Settings, derived: &Derivations) -> Value {
    json!({
        "source": settings.tariff_source,
        "applied": settings.octopus_tariff_applied,
        "gas_enabled": settings.octopus_gas_enabled,
        "import": tariff_json(&derived.import),
        "export": tariff_json(&derived.export),
        "gas": gas_json(&derived.gas, gas_unit_rate_usable(settings)),
    })
}

fn error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"ok": false, "error": message})))
}

/// `GET /api/octopus/tariff-preview` — what Energy Tariffs would be if they
/// followed Octopus, and why any part can't be.
pub async fn get_preview(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let Some(db) = state.history.lock().await.clone() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "History is not available");
    };
    let settings = Settings::load_async().await;
    let now = chrono::Utc::now().timestamp();
    match derive(db, settings.clone(), now).await {
        Ok(derived) => (
            StatusCode::OK,
            Json(json!({"ok": true, "data": status_json(&settings, &derived)})),
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

/// `POST /api/octopus/tariff-source` with `{"source": "octopus" | "manual"}`.
///
/// Octopus fills every part that can be followed now and keeps them current
/// after each sync; it is refused when none can. Manual stops following and
/// leaves the last values in place as a starting point for hand editing.
pub async fn post_source(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let want_octopus = match body.get("source").and_then(Value::as_str) {
        Some("octopus") => true,
        Some("manual") => false,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "source must be \"octopus\" or \"manual\"",
            )
        }
    };
    let now = chrono::Utc::now().timestamp();

    if !want_octopus {
        // Getting out of Octopus mode never depends on the rates or the
        // history database being readable: it only unlocks and stops following.
        if let Err(e) = Settings::update_async(|s| {
            s.tariff_source = TariffSource::Manual;
            s.octopus_tariff_applied = Default::default();
        })
        .await
        {
            return error(StatusCode::INTERNAL_SERVER_ERROR, &e);
        }
        let settings = Settings::load_async().await;
        // What could be followed is shown if it can be read; otherwise nothing.
        let nothing = Derivations {
            import: Err(NotDerivable::NotEnoughData),
            export: Err(NotDerivable::NotEnoughData),
            gas: Err(NotDerivable::NotEnoughData),
        };
        let derived = match state.history.lock().await.clone() {
            Some(db) => derive(db, settings.clone(), now).await.unwrap_or(nothing),
            None => nothing,
        };
        return (
            StatusCode::OK,
            Json(json!({"ok": true, "data": status_json(&settings, &derived)})),
        );
    }

    let Some(db) = state.history.lock().await.clone() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "History is not available");
    };
    let before = Settings::load_async().await;

    let derived = match derive(db, before.clone(), now).await {
        Ok(d) => d,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if !derived.any() {
        let reason = derived
            .import
            .as_ref()
            .err()
            .map_or_else(String::new, NotDerivable::reason);
        return error(
            StatusCode::BAD_REQUEST,
            &format!("Nothing to follow from Octopus yet: {reason}"),
        );
    }

    let applied = derived.clone();
    let result = Settings::update_async(move |s| {
        s.tariff_source = TariffSource::Octopus;
        apply_to_settings(s, &applied, now);
    })
    .await;
    if let Err(e) = result {
        return error(StatusCode::INTERNAL_SERVER_ERROR, &e);
    }
    let settings = Settings::load_async().await;
    (
        StatusCode::OK,
        Json(json!({"ok": true, "data": status_json(&settings, &derived)})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::OctopusTariffPriceRow;

    fn price(
        kind: &str,
        point: &str,
        code: &str,
        rate_type: &str,
        pence: f64,
    ) -> OctopusTariffPriceRow {
        OctopusTariffPriceRow {
            meter_kind: kind.into(),
            meter_point: point.into(),
            tariff_code: code.into(),
            valid_from: 0,
            valid_to: None,
            value_inc_vat: pence,
            rate_type: rate_type.into(),
        }
    }

    /// A state whose history holds a fixed import and export rate.
    async fn state_with_rates() -> Arc<AppState> {
        let dir = Settings::settings_dir();
        let db = HistoryDb::open(&dir.join("history.db")).unwrap();
        db.upsert_octopus_tariff_prices(&[
            price("electricity_import", "mp1", "E-1R-FIX", "standard", 24.5),
            price("electricity_import", "mp1", "E-1R-FIX", "standing", 54.86),
            price("electricity_export", "mp2", "E-1R-OUT", "standard", 15.0),
            price("gas", "g1", "G-1R-X", "standard", 6.1),
            price("gas", "g1", "G-1R-X", "standing", 29.4),
        ])
        .unwrap();
        let state = Arc::new(AppState::new());
        *state.history.lock().await = Some(Arc::new(db));
        state
    }

    fn post(source: &str) -> Json<Value> {
        Json(json!({ "source": source }))
    }

    /// Switch the source and check the switch was accepted.
    async fn choose(state: &Arc<AppState>, source: &str) {
        let (status, Json(body)) = post_source(State(state.clone()), post(source)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    fn db_of(state: &Arc<AppState>) -> Arc<HistoryDb> {
        state.history.try_lock().unwrap().clone().unwrap()
    }

    #[tokio::test]
    async fn the_preview_shows_what_could_be_followed_without_changing_anything() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            let before = Settings::load();
            let (status, Json(body)) = get_preview(State(state)).await;
            assert_eq!(status, StatusCode::OK);
            let data = &body["data"];
            assert_eq!(data["source"], "manual");
            assert_eq!(data["import"]["ok"], true);
            assert_eq!(data["import"]["tariff_code"], "E-1R-FIX");
            assert_eq!(data["import"]["slots"][0]["rate"], 0.245);
            assert_eq!(data["import"]["standing_charge_p_per_day"], 54.86);
            assert_eq!(data["export"]["slots"][0]["rate"], 0.15);
            assert_eq!(data["gas"]["ok"], true);
            assert_eq!(data["gas"]["unit_rate_used"], false, "gas units unknown");
            assert_eq!(
                Settings::load().import_tariff,
                before.import_tariff,
                "a preview writes nothing"
            );
            assert_eq!(Settings::load().tariff_source, TariffSource::Manual);
        })
        .await;
    }

    #[tokio::test]
    async fn the_preview_says_why_a_part_cannot_be_followed() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let dir = Settings::settings_dir();
            let db = HistoryDb::open(&dir.join("history.db")).unwrap();
            let state = Arc::new(AppState::new());
            *state.history.lock().await = Some(Arc::new(db));
            let (_, Json(body)) = get_preview(State(state)).await;
            assert_eq!(body["data"]["import"]["ok"], false);
            assert!(body["data"]["import"]["reason"].as_str().unwrap().len() > 5);
        })
        .await;
    }

    #[tokio::test]
    async fn the_preview_needs_a_history_database() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = Arc::new(AppState::new());
            let (status, _) = get_preview(State(state)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        })
        .await;
    }

    #[tokio::test]
    async fn choosing_octopus_fills_energy_tariffs_and_follows_from_then_on() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            let (status, Json(body)) = post_source(State(state.clone()), post("octopus")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["data"]["source"], "octopus");
            let s = Settings::load();
            assert_eq!(s.tariff_source, TariffSource::Octopus);
            assert_eq!(s.import_tariff, 0.245);
            assert_eq!(s.import_standing_charge_p_per_day, 54.86);
            assert_eq!(s.export_tariff, 0.15);
            assert_eq!(s.gas_standing_charge_p_per_day, 29.4);
            assert_eq!(s.octopus_tariff_applied.import.as_deref(), Some("E-1R-FIX"));
            assert!(s.octopus_tariff_applied.at.is_some());
        })
        .await;
    }

    #[tokio::test]
    async fn choosing_manual_keeps_the_values_and_stops_following() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            let (status, _) = post_source(State(state.clone()), post("manual")).await;
            assert_eq!(status, StatusCode::OK);
            let s = Settings::load();
            assert_eq!(s.tariff_source, TariffSource::Manual);
            assert_eq!(
                s.import_tariff, 0.245,
                "the last values stay as a starting point"
            );
            assert_eq!(
                s.octopus_tariff_applied,
                Default::default(),
                "nothing is locked"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn choosing_octopus_with_nothing_to_follow_is_refused_and_changes_nothing() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let dir = Settings::settings_dir();
            let db = HistoryDb::open(&dir.join("history.db")).unwrap();
            let state = Arc::new(AppState::new());
            *state.history.lock().await = Some(Arc::new(db));
            Settings::update(|s| s.import_tariff = 0.31).unwrap();
            let (status, Json(body)) = post_source(State(state), post("octopus")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("Nothing to follow"));
            let s = Settings::load();
            assert_eq!(s.tariff_source, TariffSource::Manual);
            assert_eq!(s.import_tariff, 0.31);
        })
        .await;
    }

    #[tokio::test]
    async fn switching_to_manual_needs_no_history_and_no_derivation() {
        // Following Octopus locks the editors. Getting out of that must never
        // depend on the rates or the history database being readable.
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            *state.history.lock().await = None;
            let (status, Json(body)) = post_source(State(state.clone()), post("manual")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let s = Settings::load();
            assert_eq!(s.tariff_source, TariffSource::Manual);
            assert_eq!(
                s.octopus_tariff_applied,
                Default::default(),
                "nothing is locked"
            );
            assert_eq!(
                s.import_tariff, 0.245,
                "the values stay as a starting point"
            );
            assert_eq!(body["data"]["source"], "manual");
        })
        .await;
    }

    #[tokio::test]
    async fn a_bad_source_is_rejected() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            for body in [json!({}), json!({"source": 3}), json!({"source": "agile"})] {
                let (status, _) = post_source(State(state.clone()), Json(body)).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
            }
            assert_eq!(Settings::load().tariff_source, TariffSource::Manual);
        })
        .await;
    }

    #[tokio::test]
    async fn a_sync_leaves_manual_tariffs_alone_even_when_octopus_rates_differ() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            Settings::update(|s| s.import_tariff = 0.31).unwrap();
            follow_after_sync(db_of(&state), chrono::Utc::now().timestamp())
                .await
                .unwrap();
            let s = Settings::load();
            assert_eq!(s.import_tariff, 0.31);
            assert_eq!(s.octopus_tariff_applied, Default::default());
        })
        .await;
    }

    #[tokio::test]
    async fn a_sync_brings_followed_tariffs_up_to_date() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            // The price cap moves: a newer record takes over.
            db_of(&state)
                .upsert_octopus_tariff_prices(&[OctopusTariffPriceRow {
                    valid_from: 1,
                    ..price("electricity_import", "mp1", "E-1R-FIX", "standard", 27.0)
                }])
                .unwrap();
            follow_after_sync(db_of(&state), chrono::Utc::now().timestamp())
                .await
                .unwrap();
            assert_eq!(Settings::load().import_tariff, 0.27);
        })
        .await;
    }

    #[tokio::test]
    async fn a_sync_with_tariff_download_errors_leaves_followed_tariffs_alone() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            let before = Settings::load();
            // A newer rate has arrived, but the download reported an error.
            db_of(&state)
                .upsert_octopus_tariff_prices(&[OctopusTariffPriceRow {
                    valid_from: 1,
                    ..price("electricity_import", "mp1", "E-1R-FIX", "standard", 27.0)
                }])
                .unwrap();
            follow_after_clean_sync(
                db_of(&state),
                chrono::Utc::now().timestamp(),
                Some("tariff sync: boom"),
            )
            .await
            .unwrap();
            let after = Settings::load();
            assert_eq!(after.import_tariff, before.import_tariff);
            assert_eq!(after.octopus_tariff_applied, before.octopus_tariff_applied);
        })
        .await;
    }

    #[tokio::test]
    async fn a_clean_sync_brings_followed_tariffs_up_to_date() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            db_of(&state)
                .upsert_octopus_tariff_prices(&[OctopusTariffPriceRow {
                    valid_from: 1,
                    ..price("electricity_import", "mp1", "E-1R-FIX", "standard", 27.0)
                }])
                .unwrap();
            follow_after_clean_sync(db_of(&state), chrono::Utc::now().timestamp(), None)
                .await
                .unwrap();
            assert_eq!(Settings::load().import_tariff, 0.27);
        })
        .await;
    }

    #[tokio::test]
    async fn a_sync_with_no_change_does_not_rewrite_the_settings_file() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            let path = Settings::settings_path();
            let before = std::fs::read_to_string(&path).unwrap();
            let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
            follow_after_sync(db_of(&state), chrono::Utc::now().timestamp() + 600)
                .await
                .unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
            assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        })
        .await;
    }

    #[tokio::test]
    async fn rates_that_stop_fitting_leave_the_saved_tariff_untouched() {
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            let followed = Settings::load();
            // The records vanish (a failed or partial download): nothing derives.
            let dir = Settings::settings_dir();
            let empty = Arc::new(HistoryDb::open(&dir.join("empty.db")).unwrap());
            follow_after_sync(empty, chrono::Utc::now().timestamp())
                .await
                .unwrap();
            let s = Settings::load();
            assert_eq!(s.import_tariff_config, followed.import_tariff_config);
            assert_eq!(s.import_tariff, followed.import_tariff);
            assert_eq!(
                s.octopus_tariff_applied.import, None,
                "released for hand entry"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn switching_to_manual_while_rates_were_being_read_wins() {
        // The rates were derived while Octopus was chosen; before they are
        // written the user switches to Manual. The write must not happen.
        crate::test_util::with_isolated_config_dir_async(|| async {
            let state = state_with_rates().await;
            choose(&state, "octopus").await;
            let settings = Settings::load();
            let derived = derive(db_of(&state), settings, chrono::Utc::now().timestamp())
                .await
                .unwrap();
            choose(&state, "manual").await;
            Settings::update(|s| s.import_tariff = 0.31).unwrap();
            let changed = apply_if_following(derived, chrono::Utc::now().timestamp())
                .await
                .unwrap();
            assert!(!changed);
            assert_eq!(Settings::load().import_tariff, 0.31);
        })
        .await;
    }
}

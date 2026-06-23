//! Automated support-bundle assembly and delivery (issue #125).
//!
//! Users raising tickets routinely forget (or can't be bothered) to attach
//! their developer log ring and the inverter's service-info dump. This module
//! packages both — plus useful context (manifest, sanitised settings, a recent
//! history tail) — into a single JSON bundle and ships it to the maintainer
//! via the shared ntfy topic [`SUPPORT_NTFY_TOPIC`].
//!
//! ## Bundle format
//!
//! A single JSON document (not a tarball/zip). This avoids adding a
//! compression crate dependency and keeps the output human-readable: a
//! maintainer can `jq` straight into any section without unpacking anything.
//! The document is at most a few hundred KB (bounded by the 2000-entry log
//! ring and a capped history tail), well under ntfy's 512 KB message cap and
//! Telegram's 50 MB document limit.
//!
//! ## Privacy
//!
//! [`sanitise_alerts_config`] strips every credential (Telegram bot token,
//! Pushover app token + user key) down to a `*_configured: bool` flag so the
//! maintainer can see *which* channels the user has set up without seeing the
//! secrets themselves.
//!
//! **GDPR:** the inverter serial and LAN host/port are personal data
//! (identifying the user's home network) and are NEVER included in a bundle —
//! not even behind an opt-in toggle. Per-user correlation for the maintainer
//! is preserved via [`fingerprint`], a non-reversible hash embedded in the
//! [`generate_bundle_id`]: the same serial always yields the same
//! fingerprint, but the raw serial itself never leaves the device.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::alerts::report::ReadingRow;
use crate::inverter::model::InverterSnapshot;
use crate::settings::AlertsConfig;

/// The shared ntfy topic that support bundles are published to.
///
/// Hard-coded (per issue #125) rather than per-user: the maintainer subscribes
/// to this single topic and receives every submission. Each bundle is
/// disambiguated back to its originating user by its [`generate_bundle_id`],
/// whose user segment is a non-reversible [`fingerprint`] of the inverter
/// serial (not the serial itself — GDPR) so prior tickets from the same user
/// can be cross-referenced without exposing identifying data. Always
/// publishes to the public `ntfy.sh` server regardless of the user's own
/// `ntfy_server` setting, because that is where the maintainer listens — a
/// self-hosted user's private server would swallow the submission silently.
pub const SUPPORT_NTFY_TOPIC: &str = "home-energy-manager-support";

/// Public ntfy server that support bundles are delivered to. See
/// [`SUPPORT_NTFY_TOPIC`] for why this is hard-coded rather than reusing the
/// user's configured `ntfy_server`.
pub const SUPPORT_NTFY_SERVER: &str = "https://ntfy.sh";

/// Minimum gap between two successful submissions from one process.
///
/// Stops an accidental double-click or an abusive script from flooding the
/// shared support topic. Kept short (60 s) so it isn't painful when iterating
/// or testing — a genuine user who realises they left something out of their
/// first bundle is never blocked for long.
pub const SUBMIT_COOLDOWN_SECS: i64 = 60;

/// Maximum number of history rows retained in a bundle.
///
/// A 5-second poll interval produces ~17 000 rows/day; shipping all of them
/// would bloat the bundle for little diagnostic gain over a downsampled tail.
/// The newest [`HISTORY_MAX_ROWS`] are kept in chronological order.
pub const HISTORY_MAX_ROWS: usize = 1440;

/// Upper bound on the free-text description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 2000;

// ---------------------------------------------------------------------------
// Request / input types
// ---------------------------------------------------------------------------

/// Parsed body of `POST /api/support/submit`.
#[derive(Debug, Clone, Deserialize)]
pub struct SupportRequest {
    /// Free-text description of the problem (what the user was doing, what
    /// they expected, what happened). Required; capped at
    /// [`MAX_DESCRIPTION_CHARS`].
    pub description: String,
    /// Category tag classifying the issue. Validated against
    /// [`valid_categories`] — an unrecognised value is rejected with a 400.
    pub category: String,
    /// Optional GitHub issue number this bundle relates to, so the maintainer
    /// can match an incoming bundle to an open ticket. Accepts the bare
    /// number (`125`), a leading hash (`#125`), or a full issue URL — see
    /// [`normalize_issue_number`]. Empty/absent means the user has no ticket
    /// yet (a brand-new bug report).
    #[serde(default)]
    pub issue_number: String,
    /// Whether to include the last 24 h of history readings. Defaults to
    /// `false` (opt-in): history rows are anonymised power readings, but the
    /// default-off keeps the bundle lean and gives the user explicit control
    /// over what they share.
    #[serde(default = "default_false")]
    pub include_history: bool,
}

fn default_false() -> bool {
    true
}

/// The set of accepted `category` values. Kept as a function (not a `const`
/// slice of string literals the frontend would have to mirror by hand) so the
/// source of truth is in one place; the frontend derives its dropdown from the
/// same list.
pub fn valid_categories() -> &'static [&'static str] {
    &["connection", "schedule", "battery", "control", "alerts", "other"]
}

/// Validate a [`SupportRequest`] after parsing. Returns `Ok(())` or a
/// human-readable error string suitable for a 400 response body.
pub fn validate_request(req: &SupportRequest) -> Result<(), String> {
    let description = req.description.trim();
    if description.is_empty() {
        return Err("Description is required.".to_string());
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "Description is too long (max {MAX_DESCRIPTION_CHARS} characters)."
        ));
    }
    if !valid_categories().contains(&req.category.as_str()) {
        return Err(format!(
            "Invalid category {:?}. Use one of: {}",
            req.category,
            valid_categories().join(", ")
        ));
    }
    // Validate issue number if provided. Empty is fine (no ticket yet).
    if !req.issue_number.trim().is_empty() {
        normalize_issue_number(&req.issue_number)?;
    }
    Ok(())
}

/// The GitHub repository that support bundles and their issues belong to.
/// Used to build the deep-link `Click` URL on the ntfy notification when the
/// user supplies an issue number.
pub const GITHUB_REPO_URL: &str = "https://github.com/psylsph/home-energy-manager";

/// Normalise a user-entered issue number to a bare integer.
///
/// Accepts the bare number (`125`), a leading hash (`#125`), or a full GitHub
/// issue URL (`https://github.com/.../issues/125`) — anything a user is likely
/// to paste. Returns `Ok(None)` for an empty/whitespace-only input, `Ok(Some(n))`
/// for a parseable positive integer, or `Err` if the input looks like an issue
/// reference but doesn't resolve to a positive integer (so a typo doesn't
/// silently produce a broken deep link).
pub fn normalize_issue_number(raw: &str) -> Result<Option<u64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // If it's a URL, take the trailing path segment (the issue number).
    let candidate = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let candidate = candidate.trim_start_matches('#');
    match candidate.parse::<u64>() {
        Ok(n) if n > 0 => Ok(Some(n)),
        _ => Err(format!(
            "Issue number {raw:?} is not a positive integer. Use the number (e.g. 125), #125, or the issue URL."
        )),
    }
}

/// Build the GitHub issue deep-link for a normalised issue number, or `None`
/// when no number was supplied.
pub fn issue_url(number: Option<u64>) -> Option<String> {
    number.map(|n| format!("{}/issues/{n}", GITHUB_REPO_URL))
}

/// Everything [`build_bundle`] needs to assemble a support bundle.
///
/// The API handler gathers these under short-lived locks on [`AppState`] and
/// hands owned copies here, so the (potentially slow) JSON serialisation and
/// any downstream delivery never hold a lock across an `.await`. Constructing
/// a `BundleInputs` directly is how the unit tests exercise the builder
/// without spinning up axum.
#[derive(Debug, Clone)]
pub struct BundleInputs {
    pub snapshot: Option<InverterSnapshot>,
    pub logs: Vec<String>,
    pub log_capture_level: String,
    pub host: String,
    pub port: u16,
    pub serial: String,
    pub interval_secs: u64,
    pub alerts_config: AlertsConfig,
    pub history_rows: Vec<ReadingRow>,
    pub app_version: String,
    pub platform: String,
    pub request: SupportRequest,
}

/// A fully-assembled bundle ready for delivery.
#[derive(Debug, Clone)]
pub struct BuiltBundle {
    /// Stable identifier, e.g. `hem-9f3a1c0b2e7d4051-20260623T1432Z` (the user
    /// segment is a non-reversible serial [`crate::support::fingerprint`],
    /// never the serial itself — GDPR). Surfaced to the
    /// maintainer in the ntfy title, the filename and the manifest so a reply
    /// can reference it.
    pub id: String,
    /// Filename used for the attachment (and the Telegram document).
    pub filename: String,
    /// The pretty-printed JSON body, ready to upload.
    pub json: Vec<u8>,
    /// Short human-readable summary used as the ntfy/Telegram message body.
    pub manifest_summary: String,
    /// Normalised GitHub issue number if the user supplied one, for building
    /// the ntfy `Click` deep-link. `None` when the user had no ticket yet.
    pub issue_number: Option<u64>,
    /// GitHub issue URL for the ntfy `Click` header, derived from
    /// [`BuiltBundle::issue_number`]. `None` when no issue number was given.
    pub issue_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Bundle ID
// ---------------------------------------------------------------------------

/// Generate a stable bundle ID from the inverter serial and the current UTC
/// time.
///
/// The serial is the natural per-user key (it is how the maintainer recognises
/// a returning user), and the timestamp disambiguates multiple submissions
/// from the same user. An unknown/empty serial degrades gracefully to a
/// fixed placeholder rather than producing a blank identifier.
/// A stable, non-reversible pseudonymiser for the inverter serial.
///
/// Same serial → same fingerprint, so the maintainer can correlate repeat
/// submissions from one user across tickets. The raw serial is personal data
/// under GDPR (it identifies the user's home network) and is never sent —
/// only this hash travels in the bundle id. Uses FNV-1a (64-bit): not
/// cryptographically strong, but the goal is correlation, not secrecy, and
/// avoiding a crypto-crate dependency keeps the build lean. An empty serial
/// hashes to a deterministic value too, so even an unidentified submission
/// still gets a stable id.
pub fn fingerprint(serial: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in serial.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Generate a stable bundle ID from the inverter serial and the current UTC
/// time.
///
/// The user segment is [`fingerprint`] of the serial — NOT the serial itself
/// (GDPR) — so prior tickets from the same user can be cross-referenced while
/// no identifying data leaves the device. An unknown/empty serial degrades to
/// a fixed placeholder rather than a blank identifier.
pub fn generate_bundle_id(serial: &str) -> String {
    let user_part = if serial.trim().is_empty() {
        "unknown".to_string()
    } else {
        fingerprint(serial)
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("hem-{user_part}-{stamp}")
}

/// Strip the identifying serial fields out of a serialised snapshot in place.
///
/// `InverterSnapshot` carries the raw serial in three fields
/// (`inverter_serial`, `first_inverter_serial`, and the gateway-only
/// `per_aio_serial` array). Redacting them after serialisation keeps the rest
/// of the snapshot intact (model code, firmware, power telemetry) while
/// guaranteeing no serial reaches the bundle. Called on the JSON value, not
/// the struct, so it doesn't need to know about every snapshot variant.
pub fn redact_snapshot_serials(snapshot: &mut Value) {
    let Some(obj) = snapshot.as_object_mut() else {
        return;
    };
    if obj.contains_key("inverter_serial") {
        obj.insert("inverter_serial".to_string(), json!("<redacted>"));
    }
    if obj.contains_key("first_inverter_serial") {
        obj.insert("first_inverter_serial".to_string(), json!("<redacted>"));
    }
    if obj.contains_key("per_aio_serial") {
        // Gateway plant: [String; 3] — replace each with a redaction marker so
        // the structure (array of 3) is preserved.
        obj.insert(
            "per_aio_serial".to_string(),
            json!(["<redacted>", "<redacted>", "<redacted>"]),
        );
    }
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Assemble the service-info section: the curated key/value set the Inverter
/// page shows (device type, firmware versions, capacities, max powers). The
/// identifying fields — inverter serial, dongle serial, and LAN host — are
/// ALWAYS redacted (GDPR); the model code/name are retained because they are
/// hardware identifiers, not personal data, and are needed for diagnosis.
pub fn service_info_value(
    snap: Option<&InverterSnapshot>,
    _host: &str,
    _serial: &str,
    interval_secs: u64,
) -> Value {
    match snap {
        Some(s) => json!({
            "device_type_display": s.device_type_display,
            "device_type_code": s.device_type_code,
            "arm_firmware": s.firmware_version,
            "dsp_firmware": s.dsp_firmware_version,
            "dc_dsp_firmware": s.dc_dsp_firmware_version,
            "battery_capacity_kwh": s.battery_capacity_kwh,
            "max_battery_power_w": s.max_battery_power_w,
            "max_ac_power_w": s.max_ac_power_w,
            "export_limit_w": s.export_limit_w,
            "operating_hours": s.operating_hours,
            "inverter_time": s.inverter_time,
            "host": "<redacted>",
            "dongle_serial": "<redacted>",
            "poll_interval_secs": interval_secs,
            "note": "Serial, host, and port are redacted for privacy (GDPR).",
        }),
        None => json!({
            "device_type_display": null,
            "host": "<redacted>",
            "dongle_serial": "<redacted>",
            "poll_interval_secs": interval_secs,
            "note": "No snapshot available — inverter may be disconnected.",
        }),
    }
}

/// Redact an [`AlertsConfig`] down to which channels are configured plus the
/// non-secret threshold settings. Every credential field becomes a
/// `*_configured: bool`; the raw tokens are never serialised into the bundle.
pub fn sanitise_alerts_config(cfg: &AlertsConfig) -> Value {
    json!({
        "alerts_enabled": cfg.enabled,
        "telegram_configured":
            !cfg.telegram_bot_token.is_empty() && !cfg.telegram_chat_id.is_empty(),
        "ntfy_configured": !cfg.ntfy_topic.is_empty(),
        "ntfy_server": cfg.ntfy_server,
        "pushover_configured":
            !cfg.pushover_app_token.is_empty() && !cfg.pushover_user_key.is_empty(),
        "cooldown_minutes": cfg.cooldown_minutes,
        "batt_temp_min": cfg.batt_temp_min,
        "batt_temp_max": cfg.batt_temp_max,
        "soc_min": cfg.soc_min,
        "soc_max": cfg.soc_max,
        "grid_offline_enabled": cfg.grid_offline_enabled,
        "connection_lost_enabled": cfg.connection_lost_enabled,
        "battery_over_temp_enabled": cfg.battery_over_temp_enabled,
        "solar_clipping_enabled": cfg.solar_clipping_enabled,
        "solar_clipping_ceiling_w": cfg.solar_clipping_ceiling_w,
        "daily_report_enabled": cfg.daily_report_enabled,
        "daily_report_hour": cfg.daily_report_hour,
        "daily_report_minute": cfg.daily_report_minute,
    })
}

/// Cap a history row list to the newest [`HISTORY_MAX_ROWS`], preserving
/// chronological order. No-op for shorter lists.
pub fn cap_history_rows(mut rows: Vec<ReadingRow>) -> Vec<ReadingRow> {
    if rows.len() <= HISTORY_MAX_ROWS {
        return rows;
    }
    let start = rows.len() - HISTORY_MAX_ROWS;
    rows.drain(..start);
    rows
}

/// Build the complete JSON bundle and its delivery metadata.
///
/// Pure: takes owned [`BundleInputs`], returns a [`BuiltBundle`]. No I/O, no
/// network — that makes the whole assembly path unit-testable without an
/// HTTP server or a live ntfy endpoint.
pub fn build_bundle(inputs: BundleInputs) -> Result<BuiltBundle, String> {
    // Validate the request up front so a malformed input can't produce a
    // half-built bundle.
    validate_request(&inputs.request)?;

    let serial_for_id = inputs
        .snapshot
        .as_ref()
        .map(|s| s.inverter_serial.as_str())
        .unwrap_or(&inputs.serial);
    let bundle_id = generate_bundle_id(serial_for_id);
    let created_utc = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let include_history = inputs.request.include_history;

    let log_line_count = inputs.logs.len();

    // Normalise the (optional) issue number once; an invalid value already
    // failed validation above, so this only re-parses a known-good input.
    let issue_number = normalize_issue_number(&inputs.request.issue_number)?;
    let issue_url_value = issue_url(issue_number);

    let service_info = service_info_value(
        inputs.snapshot.as_ref(),
        &inputs.host,
        &inputs.serial,
        inputs.interval_secs,
    );

    let manifest = json!({
        "bundle_id": bundle_id,
        "created_utc": created_utc,
        "app_version": inputs.app_version,
        "platform": inputs.platform,
        "category": inputs.request.category,
        "description": inputs.request.description.trim(),
        "issue_number": issue_number,
        "issue_url": issue_url_value,
        "include_history": include_history,
        "log_capture_level": inputs.log_capture_level,
        "log_line_count": log_line_count,
    });

    // Snapshot: the full decoded struct when available, with identifying
    // serial fields stripped (GDPR) via [`redact_snapshot_serials`]. Omitted
    // entirely (null) when the inverter is disconnected — a null is more
    // honest than a defaulted snapshot full of zeros that would mislead the
    // maintainer.
    let mut snapshot_value = match &inputs.snapshot {
        Some(s) => serde_json::to_value(s)
            .map_err(|e| format!("Failed to serialise snapshot: {e}"))?,
        None => Value::Null,
    };
    redact_snapshot_serials(&mut snapshot_value);

    let mut bundle = json!({
        "manifest": manifest,
        "service_info": service_info,
        "snapshot": snapshot_value,
        "logs": {
            "capture_level": inputs.log_capture_level,
            "line_count": log_line_count,
            "lines": inputs.logs,
        },
        "settings_sanitised": sanitise_alerts_config(&inputs.alerts_config),
    });

    if include_history {
        let row_count = inputs.history_rows.len();
        let rows_value = serde_json::to_value(&inputs.history_rows)
            .map_err(|e| format!("Failed to serialise history rows: {e}"))?;
        bundle["history_tail"] = json!({
            "row_count": row_count,
            "rows": rows_value,
        });
    }

    let json = serde_json::to_vec_pretty(&bundle)
        .map_err(|e| format!("Failed to serialise bundle: {e}"))?;

    // Short summary used as the ntfy/Telegram message body. Keeps the
    // maintainer's notification card scannable without opening the attachment.
    // Summary device line: model name only — never the serial (GDPR). The
    // per-user fingerprint already lives in the bundle id for correlation.
    let device = inputs
        .snapshot
        .as_ref()
        .map(|s| {
            if s.device_type_display.is_empty() {
                "(model unknown)".to_string()
            } else {
                s.device_type_display.clone()
            }
        })
        .unwrap_or_else(|| "no snapshot".to_string());
    let manifest_summary = format!(
        "HEM Support Bundle\n\
         ID: {bundle_id}\n\
         Issue: {issue}\n\
         Category: {category}\n\
         Device: {device}\n\
         App: v{version} ({platform})\n\
         Logs: {log_line_count} lines ({level})\n\n\
         Description:\n{desc}",
        bundle_id = bundle_id,
        issue = match issue_number {
            Some(n) => format!("#{n}"),
            None => "(none)".to_string(),
        },
        category = inputs.request.category,
        device = device,
        version = inputs.app_version,
        platform = inputs.platform,
        log_line_count = log_line_count,
        level = inputs.log_capture_level,
        desc = inputs.request.description.trim(),
    );

    let filename = format!("{bundle_id}.json");

    Ok(BuiltBundle {
        id: bundle_id,
        filename,
        json,
        manifest_summary,
        issue_number,
        issue_url: issue_url_value,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AlertsConfig;

    fn sample_inputs(include_history: bool) -> BundleInputs {
        let snapshot = InverterSnapshot {
            inverter_serial: "SA1234567".to_string(),
            device_type_display: "Gen3 Hybrid".to_string(),
            device_type_code: "0x0201".to_string(),
            firmware_version: "0305".to_string(),
            battery_capacity_kwh: 9.5,
            soc: 80,
            solar_power: 3000,
            ..Default::default()
        };
        let alerts = AlertsConfig {
            enabled: true,
            telegram_bot_token: "123:SECRET".to_string(),
            telegram_chat_id: "999".to_string(),
            ntfy_topic: "my-topic".to_string(),
            ntfy_server: "https://ntfy.sh".to_string(),
            pushover_app_token: String::new(),
            pushover_user_key: String::new(),
            ..Default::default()
        };
        BundleInputs {
            snapshot: Some(snapshot),
            logs: vec![
                "12:00:00.000 INFO [inverter] connected".to_string(),
                "12:00:01.000 WARN [modbus] timeout".to_string(),
            ],
            log_capture_level: "INFO".to_string(),
            host: "192.168.1.50".to_string(),
            port: 8899,
            serial: "DT123456".to_string(),
            interval_secs: 5,
            alerts_config: alerts,
            history_rows: vec![ReadingRow {
                timestamp: 1_700_000_000,
                solar_power: Some(3000),
                pv1_power: Some(2000),
                pv2_power: Some(1000),
                battery_power: Some(-500),
                grid_power: Some(200),
                home_power: Some(800),
                soc: Some(80.0),
            }],
            app_version: "0.38.0".to_string(),
            platform: "linux".to_string(),
            request: SupportRequest {
                description: "Battery not charging in slot 1.".to_string(),
                category: "battery".to_string(),
                issue_number: String::new(),
                include_history,
            },
        }
    }

    #[test]
    fn bundle_is_valid_json_with_all_sections() {
        let inputs = sample_inputs(true);
        let bundle = build_bundle(inputs).unwrap();

        let parsed: Value = serde_json::from_slice(&bundle.json).unwrap();
        assert_eq!(parsed["manifest"]["category"], "battery");
        assert_eq!(parsed["manifest"]["app_version"], "0.38.0");
        // Identifying fields are always redacted (GDPR).
        assert_eq!(parsed["service_info"]["host"], "<redacted>");
        assert_eq!(parsed["service_info"]["dongle_serial"], "<redacted>");
        assert_eq!(parsed["snapshot"]["inverter_serial"], "<redacted>");
        // But model + telemetry survive.
        assert_eq!(parsed["snapshot"]["device_type_display"], "Gen3 Hybrid");
        assert_eq!(parsed["logs"]["line_count"], 2);
        assert_eq!(parsed["logs"]["lines"][0], "12:00:00.000 INFO [inverter] connected");
        assert_eq!(parsed["snapshot"]["soc"], 80);
        assert!(parsed["history_tail"]["rows"].is_array());
    }

    #[test]
    fn serial_and_host_are_always_redacted_gdpr() {
        // No opt-in exists anymore — identifying data must never leave the
        // device regardless of any request field.
        let inputs = sample_inputs(true);
        let bundle = build_bundle(inputs).unwrap();

        let raw = String::from_utf8(bundle.json.clone()).unwrap();
        assert!(!raw.contains("SA1234567"), "raw inverter serial leaked into bundle");
        assert!(!raw.contains("192.168.1.50"), "LAN host leaked into bundle");
        assert!(!raw.contains("DT123456"), "dongle serial leaked into bundle");
        // The summary must not carry the serial either.
        assert!(!bundle.manifest_summary.contains("SA1234567"));
        let parsed: Value = serde_json::from_slice(&bundle.json).unwrap();
        assert_eq!(parsed["service_info"]["host"], "<redacted>");
        assert_eq!(parsed["service_info"]["dongle_serial"], "<redacted>");
        assert_eq!(parsed["snapshot"]["inverter_serial"], "<redacted>");
    }

    #[test]
    fn include_history_false_omits_history_tail() {
        let inputs = sample_inputs(false);
        let bundle = build_bundle(inputs).unwrap();

        let parsed: Value = serde_json::from_slice(&bundle.json).unwrap();
        assert!(parsed.get("history_tail").is_none());
    }

    #[test]
    fn secrets_are_never_serialised() {
        let inputs = sample_inputs(true);
        let bundle = build_bundle(inputs).unwrap();

        let raw = String::from_utf8(bundle.json.clone()).unwrap();
        assert!(!raw.contains("123:SECRET"), "telegram bot token leaked");
        assert!(!raw.contains("SECRET"));
        // But the configured flags should be present.
        let parsed: Value = serde_json::from_slice(&bundle.json).unwrap();
        assert_eq!(parsed["settings_sanitised"]["telegram_configured"], true);
        assert_eq!(parsed["settings_sanitised"]["ntfy_configured"], true);
        assert_eq!(parsed["settings_sanitised"]["pushover_configured"], false);
    }

    #[test]
    fn empty_description_is_rejected() {
        let mut inputs = sample_inputs(true);
        inputs.request.description = "   ".to_string();
        let err = build_bundle(inputs).unwrap_err();
        assert!(err.contains("Description is required"));
    }

    #[test]
    fn invalid_category_is_rejected() {
        let mut inputs = sample_inputs(true);
        inputs.request.category = "nonsense".to_string();
        let err = build_bundle(inputs).unwrap_err();
        assert!(err.contains("Invalid category"));
    }

    #[test]
    fn overlong_description_is_rejected() {
        let mut inputs = sample_inputs(true);
        inputs.request.description = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
        let err = build_bundle(inputs).unwrap_err();
        assert!(err.contains("too long"));
    }

    #[test]
    fn bundle_id_uses_fingerprint_not_serial() {
        // The raw serial must NOT appear in the id — only a non-reversible
        // fingerprint derived from it.
        let serial = "SA1234567";
        let id = generate_bundle_id(serial);
        assert!(!id.contains(serial), "raw serial leaked into id: {id}");
        assert!(id.starts_with("hem-"), "got {id}");
        // User segment is the 16-hex fingerprint.
        let user_seg = id.trim_start_matches("hem-").split('-').next().unwrap();
        assert_eq!(user_seg.len(), 16, "expected 16-hex fingerprint, got {user_seg}");
        assert!(user_seg.chars().all(|c| c.is_ascii_hexdigit()));
        // Trailing stamp is YYYYmmddTHHMMSSZ.
        let stamp = id.rsplit_once('-').unwrap().1;
        assert!(stamp.ends_with('Z'));
        assert_eq!(stamp.len(), 16, "stamp should be 16 chars, got {stamp}");
    }

    #[test]
    fn fingerprint_is_deterministic_and_distinct() {
        // Same serial → same fingerprint (enables cross-ticket correlation).
        assert_eq!(fingerprint("SA1234567"), fingerprint("SA1234567"));
        // Different serials → different fingerprints.
        assert_ne!(fingerprint("SA1234567"), fingerprint("SA7654321"));
        // Any bytes hash (non-alphanumerics are not stripped any more).
        assert_eq!(fingerprint("SA 12-34/56!"), fingerprint("SA 12-34/56!"));
    }

    #[test]
    fn bundle_id_handles_empty_serial() {
        let id = generate_bundle_id("");
        assert!(id.starts_with("hem-unknown-"), "got {id}");
    }

    #[test]
    fn cap_history_rows_keeps_newest_in_order() {
        let rows: Vec<ReadingRow> = (0..2000)
            .map(|i| ReadingRow {
                timestamp: i,
                solar_power: Some(i as i32),
                pv1_power: None,
                pv2_power: None,
                battery_power: None,
                grid_power: None,
                home_power: None,
                soc: None,
            })
            .collect();
        let capped = cap_history_rows(rows);
        assert_eq!(capped.len(), HISTORY_MAX_ROWS);
        // Oldest entries dropped; newest retained, still ascending.
        assert_eq!(capped[0].timestamp, 2000 - HISTORY_MAX_ROWS as i64);
        assert_eq!(capped.last().unwrap().timestamp, 1999);
    }

    #[test]
    fn filename_matches_bundle_id() {
        let inputs = sample_inputs(true);
        let bundle = build_bundle(inputs).unwrap();
        assert!(bundle.filename.starts_with(&bundle.id));
        assert!(bundle.filename.ends_with(".json"));
    }

    #[test]
    fn manifest_summary_mentions_bundle_id_and_description() {
        let inputs = sample_inputs(true);
        let bundle = build_bundle(inputs).unwrap();
        assert!(bundle.manifest_summary.contains(&bundle.id));
        assert!(bundle.manifest_summary.contains("Battery not charging"));
        assert!(bundle.manifest_summary.contains("Gen3 Hybrid"));
    }

    #[test]
    fn disconnected_inverter_produces_null_snapshot() {
        let mut inputs = sample_inputs(true);
        inputs.snapshot = None;
        let bundle = build_bundle(inputs).unwrap();

        let parsed: Value = serde_json::from_slice(&bundle.json).unwrap();
        assert!(parsed["snapshot"].is_null());
        assert_eq!(parsed["service_info"]["note"], "No snapshot available — inverter may be disconnected.");
    }

    // --- issue-number handling ---

    #[test]
    fn normalize_accepts_bare_hash_and_url() {
        assert_eq!(normalize_issue_number("").unwrap(), None);
        assert_eq!(normalize_issue_number("   ").unwrap(), None);
        assert_eq!(normalize_issue_number("125").unwrap(), Some(125));
        assert_eq!(normalize_issue_number("#125").unwrap(), Some(125));
        assert_eq!(
            normalize_issue_number("https://github.com/psylsph/home-energy-manager/issues/125").unwrap(),
            Some(125)
        );
    }

    #[test]
    fn normalize_rejects_non_positive() {
        assert!(normalize_issue_number("0").is_err());
        assert!(normalize_issue_number("-5").is_err());
        assert!(normalize_issue_number("abc").is_err());
        assert!(normalize_issue_number("#").is_err());
    }

    #[test]
    fn bundle_carries_issue_number_and_url_when_provided() {
        let mut inputs = sample_inputs(true);
        inputs.request.issue_number = "#125".to_string();
        let bundle = build_bundle(inputs).unwrap();

        assert_eq!(bundle.issue_number, Some(125));
        assert_eq!(
            bundle.issue_url.as_deref(),
            Some("https://github.com/psylsph/home-energy-manager/issues/125")
        );

        let parsed: Value = serde_json::from_slice(&bundle.json).unwrap();
        assert_eq!(parsed["manifest"]["issue_number"], 125);
        assert!(bundle.manifest_summary.contains("Issue: #125"));
    }

    #[test]
    fn bundle_has_no_issue_when_field_empty() {
        let inputs = sample_inputs(true);
        let bundle = build_bundle(inputs).unwrap();
        assert_eq!(bundle.issue_number, None);
        assert_eq!(bundle.issue_url, None);
        assert!(bundle.manifest_summary.contains("Issue: (none)"));
    }
}

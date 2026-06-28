//! Daily consumption report generator.
//!
//! Ports the `calculatePowerReport` + `exportPowerPDF` logic from the
//! frontend Power page into server-side Rust. Queries a full day of
//! history data, integrates power samples into kWh estimates, and
//! generates the same styled HTML report for email delivery.

use std::collections::BTreeMap;

use chrono::Timelike;

/// A single reading row from the history database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReadingRow {
    pub timestamp: i64,
    pub solar_power: Option<i32>,
    pub pv1_power: Option<i32>,
    pub pv2_power: Option<i32>,
    pub battery_power: Option<i32>,
    pub grid_power: Option<i32>,
    pub home_power: Option<i32>,
    pub soc: Option<f32>,
}

/// One bucket in a time-bucketed breakdown (1-hour buckets for a daily report).
#[derive(Debug, Clone, Default)]
pub struct Bucket {
    pub hour_label: String,
    pub solar_kwh: f64,
    pub home_kwh: f64,
    pub import_kwh: f64,
    pub export_kwh: f64,
    pub battery_charge_kwh: f64,
    pub battery_discharge_kwh: f64,
    pub soc_min: Option<f32>,
    pub soc_max: Option<f32>,
    pub soc_sum: f32,
    pub soc_count: u32,
}

fn positive_part(v: Option<i32>) -> f64 {
    f64::max(v.unwrap_or(0) as f64, 0.0)
}

fn negative_magnitude(v: Option<i32>) -> f64 {
    f64::max(-(v.unwrap_or(0) as f64), 0.0)
}

fn integrate_pair(
    a: Option<i32>,
    b: Option<i32>,
    hours: f64,
    transform: fn(Option<i32>) -> f64,
) -> f64 {
    match (a, b) {
        (None, None) => 0.0,
        (None, Some(b)) => transform(Some(b)) * hours / 1000.0,
        (Some(a), None) => transform(Some(a)) * hours / 1000.0,
        (Some(a), Some(b)) => (transform(Some(a)) + transform(Some(b))) / 2.0 * hours / 1000.0,
    }
}

fn median_interval_ms(rows: &[ReadingRow]) -> Option<f64> {
    if rows.len() < 2 {
        return None;
    }
    let mut intervals: Vec<f64> = rows
        .windows(2)
        .map(|w| (w[1].timestamp - w[0].timestamp) as f64 * 1000.0)
        .filter(|dt| *dt > 0.0)
        .collect();
    intervals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(intervals[intervals.len() / 2])
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn format_kwh(value: f64) -> String {
    if value >= 100.0 {
        format!("{:.0} kWh", value)
    } else {
        format!("{:.1} kWh", value)
    }
}

fn format_pct(value: f64) -> String {
    format!("{:.0}%", value)
}

fn format_watts(value: f64) -> String {
    if value >= 1000.0 {
        format!("{:.1} kW", value / 1000.0)
    } else {
        format!("{:.0} W", value)
    }
}

// ---------------------------------------------------------------------------
// Daily totals (plain-text summary for Telegram `/today`)
// ---------------------------------------------------------------------------

/// Aggregated daily energy totals, computed by integrating history rows.
///
/// This mirrors the integration logic in [`generate_daily_report_html`] but
/// returns structured data instead of HTML, so the Telegram `/today` summary
/// can reuse the exact same kWh math (and split cost by peak/off-peak hour).
#[derive(Debug, Default, Clone)]
pub struct DailyTotals {
    pub solar_kwh: f64,
    pub home_kwh: f64,
    pub import_kwh: f64,
    pub export_kwh: f64,
    pub battery_charge_kwh: f64,
    pub battery_discharge_kwh: f64,
    /// Per-hour buckets: `hour_start_unix_ts -> (import_kwh, export_kwh)`,
    /// used to split import cost between peak and off-peak windows.
    pub hourly_import_export: BTreeMap<i64, (f64, f64)>,
    pub soc_min: Option<f32>,
    pub soc_max: Option<f32>,
    pub row_count: usize,
}

/// Integrate a day's worth of power samples into kWh totals.
///
/// Returns `None` if there's insufficient data (fewer than 2 valid readings,
/// or no detectable sample interval).
pub fn compute_daily_totals(rows: &[ReadingRow]) -> Option<DailyTotals> {
    let valid: Vec<&ReadingRow> = rows
        .iter()
        .filter(|r| {
            r.solar_power.is_some()
                || r.battery_power.is_some()
                || r.grid_power.is_some()
                || r.home_power.is_some()
        })
        .collect();

    if valid.len() < 2 {
        return None;
    }

    let median_ms = median_interval_ms(rows)?;
    let max_gap_ms = median_ms * 3.5;

    let mut totals = DailyTotals::default();
    let mut soc_values: Vec<f32> = Vec::new();
    let mut hourly: BTreeMap<i64, (f64, f64)> = BTreeMap::new();

    for i in 0..valid.len() - 1 {
        let a = valid[i];
        let b = valid[i + 1];
        let raw_dt_ms = (b.timestamp - a.timestamp) as f64 * 1000.0;
        if raw_dt_ms <= 0.0 || raw_dt_ms > max_gap_ms {
            continue;
        }
        let hours = raw_dt_ms / 3_600_000.0;
        if hours <= 0.0 {
            continue;
        }

        totals.solar_kwh += integrate_pair(a.solar_power, b.solar_power, hours, positive_part);
        totals.home_kwh += integrate_pair(a.home_power, b.home_power, hours, positive_part);
        totals.import_kwh += integrate_pair(a.grid_power, b.grid_power, hours, positive_part);
        totals.export_kwh += integrate_pair(a.grid_power, b.grid_power, hours, negative_magnitude);
        totals.battery_charge_kwh +=
            integrate_pair(a.battery_power, b.battery_power, hours, negative_magnitude);
        totals.battery_discharge_kwh +=
            integrate_pair(a.battery_power, b.battery_power, hours, positive_part);

        let hour_start = (a.timestamp / 3600) * 3600;
        let e = hourly.entry(hour_start).or_insert((0.0, 0.0));
        e.0 += integrate_pair(a.grid_power, b.grid_power, hours, positive_part);
        e.1 += integrate_pair(a.grid_power, b.grid_power, hours, negative_magnitude);
    }

    for row in &valid {
        if let Some(soc) = row.soc {
            soc_values.push(soc);
        }
    }
    if soc_values.is_empty() {
        totals.soc_min = None;
        totals.soc_max = None;
    } else {
        totals.soc_min = Some(soc_values.iter().cloned().fold(f32::MAX, f32::min));
        totals.soc_max = Some(soc_values.iter().cloned().fold(f32::MIN, f32::max));
    }
    totals.hourly_import_export = hourly;
    totals.row_count = valid.len();
    Some(totals)
}

/// Parse a `"HH:MM"` time into minutes-since-midnight.
fn parse_hhmm(s: &str) -> Option<u16> {
    let mut it = s.split(':');
    let h: u16 = it.next()?.trim().parse().ok()?;
    let m: u16 = it.next()?.trim().parse().ok()?;
    Some(h * 60 + m)
}

/// Convert a Unix timestamp to minutes-of-day in the local timezone.
fn timestamp_to_local_minutes(ts: i64) -> u16 {
    let Some(dt) = chrono::DateTime::from_timestamp(ts, 0) else {
        return 0;
    };
    let local = dt.with_timezone(&chrono::Local);
    (local.hour() * 60 + local.minute()) as u16
}

/// Find the `[start, end)` boundaries (in minutes) of the tariff slot that
/// covers the given minute. Returns `(0, 1439)` as a safe fallback if no slot
/// matches (the caller already has a rate from `rate_for_minutes`).
fn find_window_bounds(cfg: &crate::settings::TariffConfig, minutes: u16) -> (u16, u16) {
    for slot in &cfg.slots {
        let Some(start) = parse_hhmm(&slot.start) else {
            continue;
        };
        let Some(end) = parse_hhmm(&slot.end) else {
            continue;
        };
        if end > start && minutes >= start && minutes < end {
            return (start, end);
        }
    }
    // 1439 = 23:59, the latest representable clock time (the final tariff
    // slot's inclusive end). Used purely for display in the Telegram summary
    // when the bucket doesn't match any defined window.
    (0, 1439)
}

/// Convert minutes-since-midnight to "HH:MM" string. Minutes are clamped
/// to `[0, 1439]` so `"23:59"` (1439) is the maximum representable clock
/// time — the final tariff slot's inclusive end.
fn minutes_to_hhmm(minutes: u16) -> String {
    let clamped = minutes.min(23 * 60 + 59);
    let h = clamped / 60;
    let m = clamped % 60;
    format!("{h:02}:{m:02}")
}

/// Build a concise plain-text daily summary for Telegram, including a
/// per-window cost split derived from the configured tariffs.
///
/// Returns `None` if there's insufficient data to integrate.
pub fn generate_daily_summary_text(
    rows: &[ReadingRow],
    date_str: &str,
    settings: &crate::settings::Settings,
) -> Option<String> {
    let t = compute_daily_totals(rows)?;

    // Resolve the import tariff config, falling back to the legacy flat
    // scalar when the structured config is absent.
    let imp_cfg = settings.import_tariff_config.clone().unwrap_or_default();
    let flat_import = settings.import_tariff;
    // Use the flat rate when the config has a single slot covering the whole
    // day at the same rate (or when no structured config exists).
    let use_flat_import = imp_cfg.slots.len() <= 1;

    // Export rate: use the peak slot rate from the export config, or the flat
    // export scalar. Export tariffs are typically flat (SEG doesn't vary by
    // time of day), so we use a single rate.
    let export_rate = settings
        .export_tariff_config
        .as_ref()
        .and_then(|c| c.rate_for_minutes(0))
        .unwrap_or(settings.export_tariff);

    // Accumulate import cost and per-window kWh using rate_for_minutes.
    let mut import_cost = 0.0_f64;
    // (start_min, end_min, rate, kwh) — grouped by unique rate+band.
    let mut window_kwh: Vec<(u16, u16, f64, f64)> = Vec::new();
    for (ts, (imp_kwh, _)) in &t.hourly_import_export {
        let rate = if use_flat_import {
            flat_import
        } else {
            // Convert timestamp to minutes-of-day in local timezone.
            let mins = timestamp_to_local_minutes(*ts);
            imp_cfg.rate_for_minutes(mins).unwrap_or(flat_import)
        };
        import_cost += imp_kwh * rate;

        // Find or create the per-window accumulator.
        // We approximate the window boundary by the hour bucket — each
        // hourly bucket maps to the slot that covers its start minute.
        let bucket_start_min = timestamp_to_local_minutes(*ts);
        // Find the actual slot boundaries for display.
        let (ws, we) = find_window_bounds(&imp_cfg, bucket_start_min);
        if let Some(entry) = window_kwh
            .iter_mut()
            .find(|(s, e, r, _)| *s == ws && *e == we && (*r - rate).abs() < 1e-12)
        {
            entry.3 += imp_kwh;
        } else {
            window_kwh.push((ws, we, rate, *imp_kwh));
        }
    }
    let export_income = t.export_kwh * export_rate;
    // Issue #131: the daily import cost above is the per-kWh component only.
    // UK-style tariffs also charge a flat daily standing fee (pence/day)
    // that doesn't scale with usage — add it to the import total so the
    // reported "Net cost" matches the user's actual bill. The Settings page
    // exposes this on the import side only (export SEG has no standing fee).
    let standing_charge_gbp = (settings.import_standing_charge_p_per_day.max(0.0)) / 100.0;
    let import_cost_with_standing = import_cost + standing_charge_gbp;
    let net_cost = import_cost_with_standing - export_income;
    let self_suff = if t.home_kwh > 0.0 {
        (1.0 - t.import_kwh / t.home_kwh).clamp(0.0, 1.0) * 100.0
    } else {
        0.0
    };

    let soc_range = match (t.soc_min, t.soc_max) {
        (Some(lo), Some(hi)) => format!("{}%–{}%", lo as i32, hi as i32),
        _ => "—".to_string(),
    };

    let mut msg = format!("📊 <b>Daily Summary</b> — {date_str}\n");
    msg.push_str("━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!("☀️ Solar: <b>{:.1} kWh</b>\n", t.solar_kwh));
    msg.push_str(&format!("🏠 Home: <b>{:.1} kWh</b>\n", t.home_kwh));
    msg.push_str(&format!(
        "🔋 Battery: +{:.1} / -{:.1} kWh  (SOC {})",
        t.battery_charge_kwh, t.battery_discharge_kwh, soc_range
    ));
    msg.push_str("\n━━━━━━━━━━━━━━━━━━━━━━━━\n");
    msg.push_str(&format!(
        "📥 Import: <b>{:.1} kWh</b> — £{:.2}\n",
        t.import_kwh, import_cost_with_standing
    ));
    // Show per-window breakdown when there are multiple distinct rates.
    if window_kwh.len() > 1 {
        for (ws, we, rate, kwh) in &window_kwh {
            if *kwh > 0.0 {
                msg.push_str(&format!(
                    "   ↳ {}–{} @ {:.1}p: {:.1} kWh\n",
                    minutes_to_hhmm(*ws),
                    minutes_to_hhmm(*we),
                    rate * 100.0,
                    kwh
                ));
            }
        }
    }
    // Issue #131: when a standing charge is configured, list it as a separate
    // line so the user can see where the daily fixed cost is coming from.
    // The amount is already rolled into the £{:.2} shown above for Import
    // and into the "Net cost" total below.
    if standing_charge_gbp > 0.0 {
        msg.push_str(&format!(
            "   ↳ Standing charge: <b>£{:.2}</b> ({}p/day)\n",
            standing_charge_gbp, settings.import_standing_charge_p_per_day
        ));
    }
    msg.push_str(&format!(
        "📤 Export: <b>{:.1} kWh</b> — £{:.2}\n",
        t.export_kwh, export_income
    ));
    msg.push_str(&format!("💵 Net cost: <b>£{:.2}</b>\n", net_cost));
    msg.push_str(&format!("🌱 Self-sufficiency: <b>{:.0}%</b>", self_suff));
    Some(msg)
}

/// Generate a daily consumption report as an HTML string.
///
/// Returns `None` if there's insufficient data (fewer than 2 readings).
pub fn generate_daily_report_html(rows: &[ReadingRow], date_str: &str) -> Option<String> {
    if rows.len() < 2 {
        return None;
    }

    // Filter out rows with null data for the main computation
    let valid: Vec<&ReadingRow> = rows
        .iter()
        .filter(|r| {
            r.solar_power.is_some()
                || r.battery_power.is_some()
                || r.grid_power.is_some()
                || r.home_power.is_some()
        })
        .collect();

    if valid.len() < 2 {
        return None;
    }

    let sorted = valid;
    let median_ms = median_interval_ms(rows)?;
    let max_gap_ms = median_ms * 3.5;

    // Integerate over successive pairs
    let mut solar_kwh = 0.0_f64;
    let mut home_kwh = 0.0_f64;
    let mut import_kwh = 0.0_f64;
    let mut export_kwh = 0.0_f64;
    let mut battery_charge_kwh = 0.0_f64;
    let mut battery_discharge_kwh = 0.0_f64;

    // Peak tracking
    let mut peak_solar_w = 0.0_f64;
    let mut peak_home_w = 0.0_f64;
    let mut peak_grid_import_w = 0.0_f64;
    let mut peak_grid_export_w = 0.0_f64;
    let mut peak_battery_charge_w = 0.0_f64;
    let mut peak_battery_discharge_w = 0.0_f64;

    // SOC tracking
    let mut soc_values: Vec<f32> = Vec::new();

    // Hourly buckets
    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();

    for i in 0..sorted.len() - 1 {
        let a = sorted[i];
        let b = sorted[i + 1];

        let raw_dt_ms = (b.timestamp - a.timestamp) as f64 * 1000.0;
        if raw_dt_ms <= 0.0 || raw_dt_ms > max_gap_ms {
            continue;
        }
        let hours = raw_dt_ms / 3_600_000.0;
        if hours <= 0.0 {
            continue;
        }

        let s = integrate_pair(a.solar_power, b.solar_power, hours, positive_part);
        let h = integrate_pair(a.home_power, b.home_power, hours, positive_part);
        let gi = integrate_pair(a.grid_power, b.grid_power, hours, positive_part);
        let ge = integrate_pair(a.grid_power, b.grid_power, hours, negative_magnitude);
        let bc = integrate_pair(a.battery_power, b.battery_power, hours, negative_magnitude);
        let bd = integrate_pair(a.battery_power, b.battery_power, hours, positive_part);

        solar_kwh += s;
        home_kwh += h;
        import_kwh += gi;
        export_kwh += ge;
        battery_charge_kwh += bc;
        battery_discharge_kwh += bd;

        // Update peaks
        peak_solar_w = peak_solar_w.max(positive_part(a.solar_power));
        peak_home_w = peak_home_w.max(positive_part(a.home_power));
        peak_grid_import_w = peak_grid_import_w.max(positive_part(a.grid_power));
        peak_grid_export_w = peak_grid_export_w.max(negative_magnitude(a.grid_power));
        peak_battery_charge_w = peak_battery_charge_w.max(negative_magnitude(a.battery_power));
        peak_battery_discharge_w = peak_battery_discharge_w.max(positive_part(a.battery_power));

        // Hour bucket
        let hour_start = (a.timestamp / 3600) * 3600;
        let bucket = buckets.entry(hour_start).or_insert_with(|| Bucket {
            hour_label: {
                let local = chrono::DateTime::from_timestamp(a.timestamp, 0)
                    .map(|dt| dt.with_timezone(&chrono::Local))
                    .unwrap();
                format!("{}:00", local.format("%H"))
            },
            ..Default::default()
        });
        bucket.solar_kwh += s;
        bucket.home_kwh += h;
        bucket.import_kwh += gi;
        bucket.export_kwh += ge;
        bucket.battery_charge_kwh += bc;
        bucket.battery_discharge_kwh += bd;
    }

    // SOC tracking across all rows
    for row in &sorted {
        if let Some(soc) = row.soc {
            soc_values.push(soc);
            let hour_start = (row.timestamp / 3600) * 3600;
            let bucket = buckets.entry(hour_start).or_insert_with(|| Bucket {
                hour_label: {
                    let local = chrono::DateTime::from_timestamp(row.timestamp, 0)
                        .map(|dt| dt.with_timezone(&chrono::Local))
                        .unwrap();
                    format!("{}:00", local.format("%H"))
                },
                ..Default::default()
            });
            bucket.soc_min = Some(bucket.soc_min.map_or(soc, |m| m.min(soc)));
            bucket.soc_max = Some(bucket.soc_max.map_or(soc, |m| m.max(soc)));
            bucket.soc_sum += soc;
            bucket.soc_count += 1;
        }
    }

    let net_grid_kwh = import_kwh - export_kwh;
    let soc_min = soc_values.iter().cloned().fold(f32::MAX, f32::min);
    let soc_max = soc_values.iter().cloned().fold(f32::MIN, f32::max);
    let _soc_avg = if soc_values.is_empty() {
        None
    } else {
        Some(soc_values.iter().sum::<f32>() / soc_values.len() as f32)
    };
    let solar_coverage = if home_kwh > 0.0 {
        Some(solar_kwh / home_kwh * 100.0)
    } else {
        None
    };
    let grid_dependency = if home_kwh > 0.0 {
        Some(import_kwh / home_kwh * 100.0)
    } else {
        None
    };
    // Clamp min/max to valid range
    let soc_min_val = if soc_values.is_empty() {
        None
    } else {
        Some(soc_min)
    };
    let soc_max_val = if soc_values.is_empty() {
        None
    } else {
        Some(soc_max)
    };

    let bucket_list: Vec<&Bucket> = buckets.values().collect();

    // Derived estimates
    let solar_to_home_est = f64::max(0.0, solar_kwh - export_kwh - battery_charge_kwh);
    let battery_to_home_est = f64::min(
        battery_discharge_kwh,
        f64::max(0.0, home_kwh - import_kwh - solar_to_home_est),
    );

    // ---- Generate HTML ----
    let mut html = format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<title>Consumption Report - {date_str}</title>
<style>
  :root {{ color-scheme: light; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
  body {{ margin: 0; background: #f3f4f6; color: #0f172a; }}
  .page {{ max-width: 980px; margin: 0 auto; padding: 28px; }}
  header {{ display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; margin-bottom: 22px; }}
  h1 {{ margin: 0 0 6px; font-size: 30px; letter-spacing: -0.04em; }}
  h2 {{ margin: 0 0 14px; font-size: 17px; }}
  .muted {{ color: #64748b; font-size: 13px; }}
  .grid-cards {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; margin-bottom: 18px; }}
  .card, .chart-card, .donut-card, .table-card {{ background: white; border: 1px solid #e2e8f0; border-radius: 18px; box-shadow: 0 8px 24px rgba(15, 23, 42, 0.06); }}
  .card {{ padding: 15px; }}
  .card span {{ display: block; color: #64748b; font-size: 11px; text-transform: uppercase; letter-spacing: .08em; font-weight: 800; }}
  .card strong {{ display: block; margin-top: 7px; font-size: 22px; letter-spacing: -0.04em; }}
  .chart-card, .donut-card, .table-card {{ padding: 18px; margin-bottom: 16px; page-break-inside: avoid; }}
  .charts-2 {{ display: grid; grid-template-columns: repeat(2, 1fr); gap: 14px; }}
  .donut-wrap {{ display: flex; align-items: center; gap: 18px; }}
  .donut {{ width: 132px; height: 132px; border-radius: 50%; display: grid; place-items: center; position: relative; flex: 0 0 auto; }}
  .donut::after {{ content: ''; position: absolute; inset: 26px; background: white; border-radius: 50%; }}
  .donut span {{ position: relative; z-index: 1; font-size: 13px; font-weight: 800; text-align: center; }}
  .donut-legend {{ flex: 1; display: flex; flex-direction: column; gap: 7px; font-size: 12px; }}
  .donut-legend-row {{ display: grid; grid-template-columns: 12px 1fr auto; align-items: center; gap: 8px; }}
  .swatch {{ width: 10px; height: 10px; border-radius: 3px; }}
  .grid-line {{ stroke: #e2e8f0; stroke-width: 1; }}
  .axis-label {{ fill: #64748b; font-size: 10px; font-weight: 700; }}
  .legend-label {{ fill: #334155; font-size: 12px; font-weight: 700; }}
  table {{ width: 100%%; border-collapse: collapse; font-size: 11px; }}
  th, td {{ padding: 7px 6px; border-bottom: 1px solid #e2e8f0; text-align: right; }}
  th:first-child, td:first-child {{ text-align: left; }}
  th {{ color: #475569; font-size: 10px; text-transform: uppercase; letter-spacing: .06em; }}
  @media print {{ body {{ background: white; }} .page {{ max-width: none; padding: 0; }} .card, .chart-card, .donut-card, .table-card {{ box-shadow: none; }} }}
</style>
</head>
<body>
<div class="page">
  <header>
    <div>
      <h1>Consumption Report</h1>
      <div class="muted">Home Energy Manager · {date_str}</div>
    </div>
  </header>

  <section class="grid-cards">
    <div class="card"><span>Solar generated</span><strong style="color:#d97706">{skwh}</strong></div>
    <div class="card"><span>Home consumed</span><strong style="color:#0f766e">{hkwh}</strong></div>
    <div class="card"><span>Grid import</span><strong style="color:#dc2626">{ikwh}</strong></div>
    <div class="card"><span>Grid export</span><strong style="color:#0284c7">{ekwh}</strong></div>
    <div class="card"><span>Net grid</span><strong>{nkwh}</strong></div>
    <div class="card"><span>Battery charged</span><strong style="color:#7c3aed">{bckwh}</strong></div>
    <div class="card"><span>Battery discharged</span><strong style="color:#16a34a">{bdkwh}</strong></div>
    <div class="card"><span>SOC range</span><strong>{socr}</strong></div>
    <div class="card"><span>Solar coverage</span><strong>{scov}</strong></div>
    <div class="card"><span>Grid dependency</span><strong>{gdep}</strong></div>
    <div class="card"><span>Peak home load</span><strong>{phw}</strong></div>
    <div class="card"><span>Peak import</span><strong>{piw}</strong></div>
  </section>
"#,
        date_str = escape_html(date_str),
        skwh = format_kwh(solar_kwh),
        hkwh = format_kwh(home_kwh),
        ikwh = format_kwh(import_kwh),
        ekwh = format_kwh(export_kwh),
        nkwh = format_kwh(net_grid_kwh),
        bckwh = format_kwh(battery_charge_kwh),
        bdkwh = format_kwh(battery_discharge_kwh),
        socr = match (soc_min_val, soc_max_val) {
            (Some(min), Some(max)) => format!("{}–{}%", min as u8, max as u8),
            _ => "—".to_string(),
        },
        scov = match solar_coverage {
            Some(v) => format_pct(v),
            None => "—".to_string(),
        },
        gdep = match grid_dependency {
            Some(v) => format_pct(v),
            None => "—".to_string(),
        },
        phw = format_watts(peak_home_w),
        piw = format_watts(peak_grid_import_w),
    );

    // Combined power chart
    html.push_str(&render_combined_power_chart(&sorted));
    // Bar charts
    if !bucket_list.is_empty() {
        html.push_str(&render_bar_chart(
            "Solar generation vs home load",
            &bucket_list,
            &[
                ("solar_kwh", "Solar", "#F59E0B"),
                ("home_kwh", "Home/load", "#14B8A6"),
            ],
        ));
        html.push_str(&render_bar_chart(
            "Grid import vs export",
            &bucket_list,
            &[
                ("import_kwh", "Import", "#EF4444"),
                ("export_kwh", "Export", "#38BDF8"),
            ],
        ));
        html.push_str(&render_bar_chart(
            "Battery charge vs discharge",
            &bucket_list,
            &[
                ("battery_charge_kwh", "Charge", "#8B5CF6"),
                ("battery_discharge_kwh", "Discharge", "#22C55E"),
            ],
        ));
    }

    // Donut charts
    html.push_str("<section class=\"charts-2\">");
    html.push_str(&render_donut(
        "Grid balance",
        &[
            ("Import", import_kwh, "#EF4444"),
            ("Export", export_kwh, "#38BDF8"),
        ],
    ));
    html.push_str(&render_donut(
        "Battery activity",
        &[
            ("Charge", battery_charge_kwh, "#8B5CF6"),
            ("Discharge", battery_discharge_kwh, "#22C55E"),
        ],
    ));
    html.push_str("</section>");
    html.push_str("<section class=\"charts-2\">");
    html.push_str(&render_donut(
        "Estimated solar destination",
        &[
            ("Used locally", solar_to_home_est, "#14B8A6"),
            ("Charged battery", battery_charge_kwh, "#8B5CF6"),
            ("Exported", export_kwh, "#38BDF8"),
        ],
    ));
    html.push_str(&render_donut(
        "Estimated home source",
        &[
            ("Grid import", import_kwh, "#EF4444"),
            ("Battery discharge", battery_to_home_est, "#22C55E"),
            (
                "Direct solar",
                f64::max(0.0, home_kwh - import_kwh - battery_to_home_est),
                "#F59E0B",
            ),
        ],
    ));
    html.push_str("</section>");

    // Bucket breakdown table
    html.push_str("<section class=\"table-card\"><h2>Bucket breakdown</h2><table><thead><tr>");
    html.push_str(
        "<th>Hour</th><th>Solar</th><th>Home</th><th>Import</th><th>Export</th><th>Charge</th><th>Discharge</th><th>Avg SOC</th>",
    );
    html.push_str("</tr></thead><tbody>");
    for bucket in &bucket_list {
        let avg_soc = if bucket.soc_count > 0 {
            format!("{:.0}%", bucket.soc_sum / bucket.soc_count as f32)
        } else {
            "—".to_string()
        };
        html.push_str(&format!(
            "<tr><td>{}</td><td>{:.2}</td><td>{:.2}</td><td>{:.2}</td><td>{:.2}</td><td>{:.2}</td><td>{:.2}</td><td>{}</td></tr>",
            escape_html(&bucket.hour_label),
            bucket.solar_kwh,
            bucket.home_kwh,
            bucket.import_kwh,
            bucket.export_kwh,
            bucket.battery_charge_kwh,
            bucket.battery_discharge_kwh,
            avg_soc,
        ));
    }
    html.push_str("</tbody></table></section>");

    html.push_str("</div></body></html>");
    Some(html)
}

// ---------------------------------------------------------------------------
// SVG render helpers (ports of PowerPage.tsx render functions)
// ---------------------------------------------------------------------------

fn render_combined_power_chart(sorted: &[&ReadingRow]) -> String {
    let width = 920;
    let height = 300;
    let left = 54;
    let right = 54;
    let top = 40;
    let bottom = 48;
    let chart_w = (width - left - right) as f64;
    let chart_h = (height - top - bottom) as f64;

    let min_t = sorted.first().map(|r| r.timestamp as f64).unwrap_or(0.0);
    let max_t = sorted.last().map(|r| r.timestamp as f64).unwrap_or(0.0);
    if min_t >= max_t {
        return "<section class=\"chart-card\"><h2>Combined Power Flow</h2><p class=\"muted\">Not enough data for chart.</p></section>".to_string();
    }

    let max_power = sorted
        .iter()
        .flat_map(|r| {
            let mut vals = vec![0_f64; 0];
            if let Some(v) = r.solar_power {
                vals.push(v.abs() as f64);
            }
            if let Some(v) = r.battery_power {
                vals.push(v.abs() as f64);
            }
            if let Some(v) = r.grid_power {
                vals.push(v.abs() as f64);
            }
            if let Some(v) = r.home_power {
                vals.push(v.abs() as f64);
            }
            vals
        })
        .fold(0.0_f64, f64::max)
        .max(1000.0);
    let y_max = (max_power / 1000.0).ceil() * 1000.0;

    let x_for = |t: f64| left as f64 + ((t - min_t) / (max_t - min_t).max(1.0)) * chart_w;
    let y_for_power = |v: f64| top as f64 + chart_h / 2.0 - (v / y_max) * (chart_h / 2.0);
    let y_for_soc = |v: f64| top as f64 + chart_h - (v / 100.0) * chart_h;

    let series = [
        ("#F59E0B", "solar"),
        ("#22C55E", "battery"),
        ("#EF4444", "grid"),
        ("#14B8A6", "home"),
    ];

    let mut polylines = String::new();

    for &(color, _) in &series {
        let points: Vec<String> = sorted
            .iter()
            .filter_map(|r| {
                let val = match color {
                    "#F59E0B" => r.solar_power,
                    "#22C55E" => r.battery_power,
                    "#EF4444" => r.grid_power,
                    "#14B8A6" => r.home_power,
                    _ => None,
                }?;
                Some(format!(
                    "{:.1},{:.1}",
                    x_for(r.timestamp as f64),
                    y_for_power(val as f64)
                ))
            })
            .collect();
        if !points.is_empty() {
            polylines.push_str(&format!(
                "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"2.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" />",
                points.join(" "), color
            ));
        }
    }

    // SOC line
    let soc_points: Vec<String> = sorted
        .iter()
        .filter_map(|r| {
            let val = r.soc?;
            Some(format!(
                "{:.1},{:.1}",
                x_for(r.timestamp as f64),
                y_for_soc(val as f64)
            ))
        })
        .collect();
    let soc_line = if !soc_points.is_empty() {
        format!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"#A78BFA\" stroke-width=\"2\" stroke-dasharray=\"5 4\" stroke-linecap=\"round\" stroke-linejoin=\"round\" />",
            soc_points.join(" ")
        )
    } else {
        String::new()
    };

    // Grid lines
    let mut extras = String::new();
    for &ratio in &[-1.0, -0.5, 0.0, 0.5, 1.0] {
        let y = top as f64 + chart_h / 2.0 - ratio * chart_h / 2.0;
        let value = y_max * ratio;
        extras.push_str(&format!(
            "<line x1=\"{}\" x2=\"{}\" y1=\"{:.1}\" y2=\"{:.1}\" class=\"grid-line\" />",
            left,
            width - right,
            y,
            y
        ));
        let label = if value.abs() >= 1000.0 {
            format!("{:.0}k", value / 1000.0)
        } else {
            format!("{:.0}", value)
        };
        extras.push_str(&format!(
            "<text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\" class=\"axis-label\">{}</text>",
            left - 8,
            y + 4.0,
            label
        ));
    }
    // SOC axis
    for &val in &[0.0, 50.0, 100.0] {
        let y = y_for_soc(val);
        extras.push_str(&format!(
            "<text x=\"{}\" y=\"{:.1}\" class=\"axis-label\">{:.0}%</text>",
            width - right + 8,
            y + 4.0,
            val
        ));
    }

    // Legend
    let legend_items = [
        ("#F59E0B", "Solar", ""),
        ("#22C55E", "Battery", ""),
        ("#EF4444", "Grid", ""),
        ("#14B8A6", "Home/load", ""),
        ("#A78BFA", "SOC", "5 4"),
    ];
    let mut legend = String::new();
    for (i, &(color, label, dash)) in legend_items.iter().enumerate() {
        let x = left as f64 + i as f64 * 135.0;
        if dash.is_empty() {
            legend.push_str(&format!(
                "<g><line x1=\"{:.1}\" x2=\"{:.1}\" y1=\"18\" y2=\"18\" stroke=\"{}\" stroke-width=\"3\" />",
                x, x + 20.0, color
            ));
        } else {
            legend.push_str(&format!(
                "<g><line x1=\"{:.1}\" x2=\"{:.1}\" y1=\"18\" y2=\"18\" stroke=\"{}\" stroke-width=\"3\" stroke-dasharray=\"{}\" />",
                x, x + 20.0, color, dash
            ));
        }
        legend.push_str(&format!(
            "<text x=\"{:.1}\" y=\"22\" class=\"legend-label\">{}</text></g>",
            x + 28.0,
            escape_html(label)
        ));
    }

    let start_label = chrono::DateTime::from_timestamp(min_t as i64, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%d %b").to_string())
        .unwrap_or_default();
    let end_label = chrono::DateTime::from_timestamp(max_t as i64, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%d %b").to_string())
        .unwrap_or_default();

    format!(
        "<section class=\"chart-card\"><h2>Combined Power Flow</h2><svg viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"Combined Power Flow\">{legend}{extras}<line x1=\"{left}\" x2=\"{left}\" y1=\"{top}\" y2=\"{}\" class=\"grid-line\" /><line x1=\"{}\" x2=\"{}\" y1=\"{top}\" y2=\"{}\" class=\"grid-line\" />{polylines}{soc_line}<text x=\"{left}\" y=\"{}\" class=\"axis-label\">{}</text><text x=\"{}\" y=\"{}\" text-anchor=\"end\" class=\"axis-label\">{}</text></svg></section>",
        top as f64 + chart_h,
        width - right, width - right, top as f64 + chart_h,
        height - 16, start_label,
        width - right, height - 16, end_label,
    )
}

fn render_bar_chart(title: &str, buckets: &[&Bucket], series: &[(&str, &str, &str)]) -> String {
    let width = 920;
    let height = 280;
    let left = 54;
    let right = 18;
    let top = 36;
    let bottom = 54;
    let chart_w = (width - left - right) as f64;
    let chart_h = (height - top - bottom) as f64;

    let max_val = buckets
        .iter()
        .flat_map(|b| {
            series.iter().map(|(key, _, _)| {
                let v = match *key {
                    "solar_kwh" => b.solar_kwh,
                    "home_kwh" => b.home_kwh,
                    "import_kwh" => b.import_kwh,
                    "export_kwh" => b.export_kwh,
                    "battery_charge_kwh" => b.battery_charge_kwh,
                    "battery_discharge_kwh" => b.battery_discharge_kwh,
                    _ => 0.0,
                };
                if v > 0.0 {
                    v
                } else {
                    0.0
                }
            })
        })
        .fold(0.1_f64, f64::max);

    let group_w = chart_w / (buckets.len().max(1) as f64);
    let bar_w = (2.0_f64).max(18.0_f64.min(group_w / ((series.len() + 1) as f64)));
    let label_every = (buckets.len() as f64 / 12.0).ceil().max(1.0) as usize;

    let mut bars = String::new();
    for (bi, bucket) in buckets.iter().enumerate() {
        for (si, &(key, _, color)) in series.iter().enumerate() {
            let v = match key {
                "solar_kwh" => bucket.solar_kwh,
                "home_kwh" => bucket.home_kwh,
                "import_kwh" => bucket.import_kwh,
                "export_kwh" => bucket.export_kwh,
                "battery_charge_kwh" => bucket.battery_charge_kwh,
                "battery_discharge_kwh" => bucket.battery_discharge_kwh,
                _ => 0.0,
            };
            let bar_h = v / max_val * chart_h;
            let x = left as f64
                + bi as f64 * group_w
                + (group_w - bar_w * series.len() as f64) / 2.0
                + si as f64 * bar_w;
            let y = top as f64 + chart_h - bar_h;
            bars.push_str(&format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"2\" fill=\"{}\" />",
                x, y, bar_w, bar_h, color
            ));
        }
    }

    let mut labels = String::new();
    for (i, bucket) in buckets.iter().enumerate() {
        if i % label_every != 0 && i != buckets.len() - 1 {
            continue;
        }
        let x = left as f64 + i as f64 * group_w + group_w / 2.0;
        labels.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{}\" text-anchor=\"middle\" class=\"axis-label\">{}</text>",
            x,
            height - 18,
            escape_html(&bucket.hour_label)
        ));
    }

    let mut grid = String::new();
    for &ratio in &[0.0, 0.25, 0.5, 0.75, 1.0] {
        let y = top as f64 + chart_h - ratio * chart_h;
        let val = max_val * ratio;
        grid.push_str(&format!(
            "<line x1=\"{}\" x2=\"{}\" y1=\"{:.1}\" y2=\"{:.1}\" class=\"grid-line\" />",
            left,
            width - right,
            y,
            y
        ));
        grid.push_str(&format!(
            "<text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\" class=\"axis-label\">{:.0}</text>",
            left - 8,
            y + 4.0,
            val
        ));
    }

    let mut legend = String::new();
    for (i, &(_, label, color)) in series.iter().enumerate() {
        let x = left as f64 + i as f64 * 150.0;
        legend.push_str(&format!(
            "<g><rect x=\"{:.1}\" y=\"14\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{}\" />",
            x, color
        ));
        legend.push_str(&format!(
            "<text x=\"{:.1}\" y=\"23\" class=\"legend-label\">{}</text></g>",
            x + 16.0,
            escape_html(label)
        ));
    }

    format!(
        "<section class=\"chart-card\"><h2>{}</h2><svg viewBox=\"0 0 {width} {height}\" role=\"img\" aria-label=\"{}\">{}{}{}{}</svg></section>",
        escape_html(title), escape_html(title), legend, grid, bars, labels
    )
}

fn render_donut(title: &str, items: &[(&str, f64, &str)]) -> String {
    let total: f64 = items.iter().map(|(_, v, _)| f64::max(*v, 0.0)).sum();
    let mut cursor = 0.0;
    let stops: String = if total > 0.0 {
        items
            .iter()
            .map(|(_, v, color)| {
                let start = cursor;
                let degrees = f64::max(*v, 0.0) / total * 360.0;
                cursor += degrees;
                format!("{} {:.1}deg {:.1}deg", color, start, cursor)
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        "#30363d 0deg 360deg".to_string()
    };

    let legend_html: String = items
        .iter()
        .map(|(label, v, color)| {
            format!(
                "<div class=\"donut-legend-row\"><span class=\"swatch\" style=\"background:{}\"></span><span>{}</span><strong>{}</strong></div>",
                color, escape_html(label), format_kwh(*v)
            )
        })
        .collect();

    format!(
        "<section class=\"donut-card\"><h2>{}</h2><div class=\"donut-wrap\"><div class=\"donut\" style=\"background: conic-gradient({});\"><span>{}</span></div><div class=\"donut-legend\">{}</div></div></section>",
        escape_html(title), stops, if total > 0.0 { format_kwh(total) } else { "0 kWh".to_string() }, legend_html
    )
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_reading(
        ts: i64,
        solar: i32,
        battery: i32,
        grid: i32,
        home: i32,
        soc: f32,
    ) -> ReadingRow {
        ReadingRow {
            timestamp: ts,
            solar_power: Some(solar),
            pv1_power: None,
            pv2_power: None,
            battery_power: Some(battery),
            grid_power: Some(grid),
            home_power: Some(home),
            soc: Some(soc),
        }
    }

    #[test]
    fn test_insufficient_data_returns_none() {
        let rows = vec![dummy_reading(0, 0, 0, 0, 0, 50.0)];
        assert!(generate_daily_report_html(&rows, "2026-06-17").is_none());
    }

    #[test]
    fn test_generates_html_with_two_readings() {
        let rows = vec![
            dummy_reading(0, 1000, 200, -500, 800, 50.0),
            dummy_reading(3600, 2000, -300, -1000, 2500, 60.0),
        ];
        let html = generate_daily_report_html(&rows, "2026-06-17");
        assert!(html.is_some());
        let html = html.unwrap();
        assert!(html.contains("Consumption Report"));
        assert!(html.contains("2026-06-17"));
        assert!(html.contains("<table"));
    }

    #[test]
    fn test_combined_power_chart_generated() {
        let rows = vec![
            dummy_reading(0, 1000, 200, -500, 800, 50.0),
            dummy_reading(1800, 1500, 100, -700, 1200, 55.0),
            dummy_reading(3600, 2000, -300, -1000, 2500, 60.0),
        ];
        let html = generate_daily_report_html(&rows, "2026-06-17").unwrap();
        assert!(html.contains("Combined Power Flow"));
        assert!(html.contains("<polyline"));
        assert!(html.contains("svg"));
    }

    #[test]
    fn test_positive_part() {
        assert!((positive_part(Some(5)) - 5.0).abs() < 0.001);
        assert!((positive_part(Some(-5))).abs() < 0.001);
        assert!((positive_part(None)).abs() < 0.001);
    }

    #[test]
    fn test_negative_magnitude() {
        assert!((negative_magnitude(Some(-5)) - 5.0).abs() < 0.001);
        assert!((negative_magnitude(Some(5))).abs() < 0.001);
        assert!((negative_magnitude(None)).abs() < 0.001);
    }

    #[test]
    fn test_integrate_pair_trapezoid() {
        // 1 hour at average 1000 W = 1 kWh
        let kwh = integrate_pair(Some(1000), Some(1000), 1.0, positive_part);
        assert!((kwh - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_integrate_pair_ramp() {
        // 1 hour, starts at 0, ends at 2000 → avg 1000 → 1 kWh
        let kwh = integrate_pair(Some(0), Some(2000), 1.0, positive_part);
        assert!((kwh - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_html_escapes_special_chars() {
        assert_eq!(escape_html("<script>"), "&lt;script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
        assert_eq!(escape_html("hello"), "hello");
    }

    #[test]
    fn test_compute_daily_totals_matches_integration() {
        // Same fixture as the HTML report test: 1 hour from 0 → 2000 W solar
        // should integrate to ~1 kWh (trapezoid), confirming the totals helper
        // reuses the same math as generate_daily_report_html.
        let rows = vec![
            dummy_reading(0, 0, 0, 0, 0, 50.0),
            dummy_reading(3600, 2000, 0, 0, 0, 60.0),
        ];
        let t = compute_daily_totals(&rows).expect("should integrate two readings");
        assert!((t.solar_kwh - 1.0).abs() < 0.001, "solar = {}", t.solar_kwh);
        assert_eq!(t.row_count, 2);
        assert_eq!(t.soc_min, Some(50.0));
        assert_eq!(t.soc_max, Some(60.0));
        assert_eq!(t.hourly_import_export.len(), 1);
    }

    #[test]
    fn test_compute_daily_totals_insufficient_data() {
        assert!(compute_daily_totals(&[]).is_none());
        assert!(compute_daily_totals(&[dummy_reading(0, 0, 0, 0, 0, 0.0)]).is_none());
    }

    #[test]
    fn test_off_peak_window_via_rate_for_minutes() {
        // Off-peak 00:30–05:30 via the new slots model (migrated from legacy).
        let cfg = crate::settings::TariffConfig::default();
        let midday_mins = 12 * 60; // 720
        let night_mins = 3 * 60; // 180
                                 // Peak at midday, off-peak at night.
        assert!(
            cfg.rate_for_minutes(midday_mins).unwrap() > cfg.rate_for_minutes(night_mins).unwrap()
        );
    }

    #[test]
    fn test_midnight_crossing_via_legacy_migration() {
        // 23:00–05:30 crosses midnight. Migrate legacy fields to slots and
        // verify the rate at every relevant minute.
        let legacy = r#"{
            "peak_rate": 0.30,
            "off_peak_rate": 0.10,
            "off_peak_start": "23:00",
            "off_peak_end": "05:30"
        }"#;
        let cfg: crate::settings::TariffConfig = serde_json::from_str(legacy).unwrap();
        // 23:30 → off-peak
        assert_eq!(cfg.rate_for_minutes(23 * 60 + 30), Some(0.10));
        // 03:00 → off-peak (past midnight)
        assert_eq!(cfg.rate_for_minutes(3 * 60), Some(0.10));
        // 12:00 → peak
        assert_eq!(cfg.rate_for_minutes(12 * 60), Some(0.30));
    }

    // -----------------------------------------------------------------------
    // Issue #131: standing charge in the daily consumption report.
    //
    // The report must roll the configured import-side standing charge into
    // both the import cost and the net-cost lines, AND surface a labelled
    // footnote so the user can see where the daily fixed cost is coming
    // from. When no standing charge is configured, the message must look
    // exactly as it did before (no stray footnote).
    // -----------------------------------------------------------------------

    /// Build a minimal Settings struct with `import_tariff_config` set to a
    /// single slot covering the whole day at `flat_import` £/kWh. Mirrors
    /// the default behaviour for users who haven't migrated to a ToU
    /// tariff config. Avoids a `field_reassign_with_default` clippy lint by
    /// building the struct literal with explicit fields rather than
    /// post-construction reassignment.
    fn settings_with_flat_import(flat_import: f64) -> crate::settings::Settings {
        crate::settings::Settings {
            import_tariff: flat_import,
            import_tariff_config: Some(crate::settings::TariffConfig::flat(flat_import)),
            ..crate::settings::Settings::default()
        }
    }

    /// Two readings that, integrated, give 1 kWh of grid import at +1000 W.
    /// The positive grid value reflects the inverter's "imported from grid"
    /// convention; `positive_part` in this codebase treats positive grid as
    /// import. The trapezoid integration between t=0 and t=3600 at +1000 W
    /// yields exactly 1.0 kWh.
    fn import_rows() -> Vec<ReadingRow> {
        vec![
            dummy_reading(0, 0, 0, 1000, 1000, 50.0),
            dummy_reading(3600, 0, 0, 1000, 1000, 60.0),
        ]
    }

    #[test]
    fn test_daily_report_no_standing_charge_omits_footnote() {
        // Baseline: when `import_standing_charge_p_per_day` is 0 (the
        // default), the report must NOT add a "Standing charge" line. This
        // guards against regression for users who haven't configured one.
        let s = settings_with_flat_import(0.25);
        let msg =
            generate_daily_summary_text(&import_rows(), "2026-06-27", &s).expect("enough data");
        assert!(
            !msg.contains("Standing charge"),
            "report must not show a standing-charge footnote when none is configured"
        );
        // And the import line should still be £0.25 (1 kWh × £0.25).
        assert!(
            msg.contains("£0.25"),
            "import cost should still be the per-kWh component only; got: {msg}"
        );
    }

    #[test]
    fn test_daily_report_includes_standing_charge_in_net_cost() {
        // With a 54.86p/day standing charge, the net cost must equal
        // per-kWh cost (£0.25) + standing (£0.5486) = £0.7986, rounded to
        // 2dp. This is the headline behaviour the user asked for: a
        // bill-accurate total.
        let mut s = settings_with_flat_import(0.25);
        s.import_standing_charge_p_per_day = 54.86;
        let msg =
            generate_daily_summary_text(&import_rows(), "2026-06-27", &s).expect("enough data");
        assert!(
            msg.contains("Net cost: <b>£0.80</b>"),
            "net cost should include the standing charge (0.25 + 0.5486 ≈ 0.80); got: {msg}"
        );
    }

    #[test]
    fn test_daily_report_emits_standing_charge_footnote_line() {
        // The footnote line is the user-facing affordance: it tells them
        // the daily fixed cost exists and where it came from.
        let mut s = settings_with_flat_import(0.25);
        s.import_standing_charge_p_per_day = 54.86;
        let msg =
            generate_daily_summary_text(&import_rows(), "2026-06-27", &s).expect("enough data");
        assert!(
            msg.contains("Standing charge:"),
            "expected a labelled standing-charge footnote; got: {msg}"
        );
        // The 2dp value (£0.55).
        assert!(
            msg.contains("£0.55"),
            "footnote should show the standing charge rounded to 2dp; got: {msg}"
        );
        // And the originating p/day figure so the user can verify the
        // setting they entered.
        assert!(
            msg.contains("54.86p/day"),
            "footnote should show the configured p/day figure; got: {msg}"
        );
    }

    #[test]
    fn test_daily_report_negative_standing_charge_is_clamped() {
        // A negative standing charge is nonsensical (no tariff credits a
        // daily fee). The implementation clamps to 0 — the resulting
        // report must look like the no-standing-charge baseline.
        let mut s = settings_with_flat_import(0.25);
        s.import_standing_charge_p_per_day = -10.0;
        let msg =
            generate_daily_summary_text(&import_rows(), "2026-06-27", &s).expect("enough data");
        assert!(
            !msg.contains("Standing charge"),
            "negative standing charge must be clamped (no footnote expected); got: {msg}"
        );
        assert!(
            msg.contains("Net cost: <b>£0.25</b>"),
            "net cost should equal the per-kWh component only (got: {msg})"
        );
    }
}

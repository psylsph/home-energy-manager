//! Working out Energy Tariffs from the rates Octopus charges (issue #455).
//!
//! HEM already downloads each agreement's dated unit rates and standing
//! charges (`octopus_tariff_prices`). This turns them into the fixed daily
//! windows Settings → Energy Tariffs holds, when the tariff can be expressed
//! that way, and says why when it can't. It is pure: no I/O, and "now" and the
//! zone are arguments, so every case is deterministic.
//!
//! A tariff fits when its unit rate follows the same windows every day.
//! Fixed, variable, Tracker (one rate that changes daily), Go and Intelligent
//! Go, Cosy, Flux and Economy 7 all do. Agile changes every half hour and
//! doesn't; no product code is consulted, the shape of the rates decides.

use std::collections::{BTreeSet, HashMap};

use chrono::{Duration, TimeZone, Timelike};
use chrono_tz::Tz;

use crate::history::reprice::{newest_covering, Part, Span};
use crate::settings::{TariffConfig, TariffSlot};

/// More windows than this in a day isn't a fixed-window tariff.
pub const MAX_DERIVED_WINDOWS: usize = 12;

/// How many local days ending today are searched for complete ones.
const SAMPLE_DAYS: i64 = 7;

/// Two rates this close (pence) are the same rate.
const SAME_RATE_PENCE: f64 = 1e-9;

/// One dated rate record, as stored in `octopus_tariff_prices`.
#[derive(Debug, Clone, PartialEq)]
pub struct RateRecord {
    pub meter_point: String,
    pub tariff_code: String,
    /// `standard`, `day`, `night` or `standing`.
    pub rate_type: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    /// Pence per kWh, or per day for a standing charge, VAT included.
    pub pence: f64,
}

/// Energy Tariffs worked out from the supplier's rates for one direction.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedTariff {
    pub meter_point: String,
    pub tariff_code: String,
    /// Windows tiling `[00:00, 23:59]`, rates in £/kWh.
    pub config: TariffConfig,
    /// Pence per day, when the supplier's rates include one.
    pub standing_charge_p_per_day: Option<f64>,
}

/// A gas agreement's flat rates.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedGas {
    pub meter_point: String,
    pub tariff_code: String,
    pub unit_rate_p_per_kwh: f64,
    pub standing_charge_p_per_day: Option<f64>,
}

/// Why a tariff can't be filled in from the supplier's rates.
#[derive(Debug, Clone, PartialEq)]
pub enum NotDerivable {
    /// No agreement has rates covering now.
    NoCurrentAgreement,
    /// More than one agreement covers now; which is the house's isn't known.
    Ambiguous,
    /// Not a single complete day of rates to read the pattern from.
    NotEnoughData,
    /// The rates change more often than a fixed daily window can say.
    TooManyWindows(usize),
    /// The windows were different on the last two days.
    VariesDaily,
    /// The windows came out but aren't a valid tariff.
    Invalid(String),
}

impl NotDerivable {
    /// A reason to show the user, in a sentence.
    pub fn reason(&self) -> String {
        match self {
            Self::NoCurrentAgreement => "Octopus has no rates for your current agreement".into(),
            Self::Ambiguous => "More than one Octopus agreement is current".into(),
            Self::NotEnoughData => "Not enough Octopus rates downloaded yet".into(),
            Self::TooManyWindows(n) => format!(
                "The rates change {n} times a day, too often for fixed windows (as on Agile)"
            ),
            Self::VariesDaily => "The rate windows changed shape in the last two days".into(),
            Self::Invalid(why) => format!("The Octopus rates don't make a valid tariff: {why}"),
        }
    }
}

/// The records of one agreement, grouped by what they price.
struct Agreement {
    meter_point: String,
    tariff_code: String,
    parts: HashMap<Part, Vec<Span>>,
}

impl Agreement {
    fn covering(&self, part: Part, ts: i64) -> Option<f64> {
        let spans = self.parts.get(&part)?;
        newest_covering(spans, ts).map(|s| s.pence)
    }
}

/// The hours (minutes of the UK day, start and end) a tariff's `night` rate
/// applies, for tariffs that publish a day and a night rate without saying
/// when. `economy7` is the household's Economy 7 window.
///
/// Intelligent Octopus Go's off-peak hours are fixed by the product, 23:30 to
/// 05:30 UK time (Octopus's own earlier Intelligent Go rates list exactly those
/// hours), and are not the household's Economy 7 hours. Every other tariff
/// uses the Economy 7 window, as the Octopus page's billing always has.
pub fn night_window_for(tariff_code: &str, economy7: (u16, u16)) -> (u16, u16) {
    if tariff_code.contains("-IOG-") || tariff_code.contains("-INTELLI-") {
        (23 * 60 + 30, 5 * 60 + 30)
    } else {
        economy7
    }
}

/// Whether `minute` of the UK day falls in the night hours `(start, end)`,
/// which wrap past midnight when `start` is not before `end` (a window with
/// `start == end` is the whole day).
pub fn in_night_window(minute: u16, (start, end): (u16, u16)) -> bool {
    if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

/// The household's Economy 7 night hours from settings, in minutes of the UK
/// day, with the usual 00:30–07:30 for anything unparseable.
pub fn economy7_window(settings: &crate::settings::Settings) -> (u16, u16) {
    (
        crate::settings::parse_hhmm_to_minutes(&settings.octopus_economy7_start).unwrap_or(30),
        crate::settings::parse_hhmm_to_minutes(&settings.octopus_economy7_end).unwrap_or(450),
    )
}

/// The one agreement whose records cover `now`.
fn current_agreement(records: &[RateRecord], now: i64) -> Result<Agreement, NotDerivable> {
    let live = |r: &&RateRecord| r.valid_from <= now && r.valid_to.is_none_or(|to| to > now);
    let agreements: BTreeSet<(&str, &str)> = records
        .iter()
        .filter(live)
        .map(|r| (r.meter_point.as_str(), r.tariff_code.as_str()))
        .collect();
    let mut it = agreements.into_iter();
    let (Some((meter_point, tariff_code)), None) = (it.next(), it.next()) else {
        return Err(if records.iter().any(|r| live(&r)) {
            NotDerivable::Ambiguous
        } else {
            NotDerivable::NoCurrentAgreement
        });
    };
    let mut parts: HashMap<Part, Vec<Span>> = HashMap::new();
    for r in records {
        if r.meter_point != meter_point || r.tariff_code != tariff_code {
            continue;
        }
        let Some(part) = Part::parse(&r.rate_type) else {
            continue;
        };
        if !r.pence.is_finite() || r.pence < 0.0 {
            continue;
        }
        parts.entry(part).or_default().push(Span {
            valid_from: r.valid_from,
            valid_to: r.valid_to,
            pence: r.pence,
            // Unused here: the hours come from `night_window_for`.
            night: (0, 0),
        });
    }
    for spans in parts.values_mut() {
        spans.sort_by_key(|s| s.valid_from);
    }
    Ok(Agreement {
        meter_point: meter_point.to_string(),
        tariff_code: tariff_code.to_string(),
        parts,
    })
}

/// The unit rate (pence/kWh) for each minute of the local day starting at
/// `day_start`, or `None` when the day has any other length than 24 hours
/// (a clock change) or any minute has no rate.
fn day_rates(
    agreement: &Agreement,
    day_start: i64,
    day_end: i64,
    night: (u16, u16),
) -> Option<Vec<f64>> {
    if day_end - day_start != 86_400 {
        return None;
    }
    let has_standard = agreement.parts.contains_key(&Part::Standard);
    (0..1440_i64)
        .map(|minute| {
            let ts = day_start + minute * 60;
            if has_standard {
                return agreement.covering(Part::Standard, ts);
            }
            let is_night = in_night_window(minute as u16, night);
            agreement.covering(if is_night { Part::Night } else { Part::Day }, ts)
        })
        .collect()
}

/// `(start minute, pence)` for each run of equal rates across the day.
fn windows_of(minutes: &[f64]) -> Vec<(u16, f64)> {
    let mut windows: Vec<(u16, f64)> = Vec::new();
    for (minute, &pence) in minutes.iter().enumerate() {
        match windows.last() {
            Some(&(_, last)) if (last - pence).abs() < SAME_RATE_PENCE => {}
            _ => windows.push((minute as u16, pence)),
        }
    }
    windows
}

fn hhmm(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// Pounds from pence, to six decimal places (the rates have at most four
/// decimal places in pence).
fn pounds(pence: f64) -> f64 {
    (pence * 10_000.0).round() / 1_000_000.0
}

/// Work out the daily windows for one direction from its rate `records`.
///
/// The records are for one meter kind (import or export). The most recent
/// complete local day, among the last seven ending today, gives the windows;
/// they must repeat the day before's. `night` is the Economy 7 night window in
/// minutes of the UK day, used only when the rates are split into day and
/// night.
pub fn derive_tariff(
    records: &[RateRecord],
    now: i64,
    night: (u16, u16),
    tz: &Tz,
) -> Result<DerivedTariff, NotDerivable> {
    let agreement = current_agreement(records, now)?;
    let has_unit_rates = agreement.parts.contains_key(&Part::Standard)
        || (agreement.parts.contains_key(&Part::Day) && agreement.parts.contains_key(&Part::Night));
    if !has_unit_rates {
        return Err(NotDerivable::NotEnoughData);
    }

    let today = chrono::DateTime::from_timestamp(now, 0)
        .ok_or(NotDerivable::NotEnoughData)?
        .with_timezone(tz)
        .date_naive();
    let midnight = |date: chrono::NaiveDate| {
        tz.from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
            .earliest()
            .map(|t| t.timestamp())
    };
    let mut complete: Vec<Vec<f64>> = Vec::new();
    for back in 0..SAMPLE_DAYS {
        let date = today - Duration::days(back);
        let (Some(start), Some(end)) = (midnight(date), midnight(date + Duration::days(1))) else {
            continue;
        };
        let hours = night_window_for(&agreement.tariff_code, night);
        if let Some(minutes) = day_rates(&agreement, start, end, hours) {
            complete.push(minutes);
            if complete.len() == 2 {
                break;
            }
        }
    }
    let latest = complete.first().ok_or(NotDerivable::NotEnoughData)?;
    let windows = windows_of(latest);
    if windows.len() > MAX_DERIVED_WINDOWS {
        return Err(NotDerivable::TooManyWindows(windows.len()));
    }
    if let Some(previous) = complete.get(1) {
        // Boundaries, not values: a price-cap change moves the values.
        let boundaries = |w: &[(u16, f64)]| w.iter().map(|(m, _)| *m).collect::<Vec<_>>();
        if boundaries(&windows_of(previous)) != boundaries(&windows) {
            return Err(NotDerivable::VariesDaily);
        }
    }

    let slots = windows
        .iter()
        .enumerate()
        .map(|(i, &(start, pence))| TariffSlot {
            start: hhmm(start),
            // The last window ends at 23:59, inclusive, as saved tariffs do.
            end: windows
                .get(i + 1)
                .map_or("23:59".to_string(), |w| hhmm(w.0)),
            rate: pounds(pence),
        })
        .collect();
    let config = TariffConfig { slots };
    config.validate().map_err(NotDerivable::Invalid)?;

    Ok(DerivedTariff {
        standing_charge_p_per_day: agreement.covering(Part::Standing, now),
        meter_point: agreement.meter_point,
        tariff_code: agreement.tariff_code,
        config,
    })
}

/// A gas agreement's unit rate (pence/kWh) and standing charge (pence/day) at
/// `now`. Gas has no time-of-use pattern, so the rate in force now is taken.
pub fn derive_gas(records: &[RateRecord], now: i64) -> Result<DerivedGas, NotDerivable> {
    let agreement = current_agreement(records, now)?;
    let unit = agreement
        .covering(Part::Standard, now)
        .ok_or(NotDerivable::NotEnoughData)?;
    Ok(DerivedGas {
        standing_charge_p_per_day: agreement.covering(Part::Standing, now),
        meter_point: agreement.meter_point,
        tariff_code: agreement.tariff_code,
        unit_rate_p_per_kwh: unit,
    })
}

/// What deriving each part of Energy Tariffs from the supplier's rates gave.
#[derive(Debug, Clone, PartialEq)]
pub struct Derivations {
    pub import: Result<DerivedTariff, NotDerivable>,
    pub export: Result<DerivedTariff, NotDerivable>,
    pub gas: Result<DerivedGas, NotDerivable>,
}

impl Derivations {
    /// Whether any part could be followed.
    pub fn any(&self) -> bool {
        self.import.is_ok() || self.export.is_ok() || self.gas.is_ok()
    }
}

/// How far back rate records are loaded: well past the week sampled, and
/// anything still current is loaded whatever its age.
const RECORD_LOOKBACK_SECS: i64 = 9 * 86_400;

/// Derive every part from the rates in `db`, as at `now`.
pub fn derive_all(
    db: &crate::history::HistoryDb,
    settings: &crate::settings::Settings,
    now: i64,
) -> Result<Derivations, String> {
    let since = now - RECORD_LOOKBACK_SECS;
    let night = economy7_window(settings);
    let import = db.octopus_rate_records("electricity_import", since)?;
    let export = db.octopus_rate_records("electricity_export", since)?;
    let gas = db.octopus_rate_records("gas", since)?;
    Ok(Derivations {
        import: derive_tariff(&import, now, night, &chrono_tz::Europe::London),
        export: derive_tariff(&export, now, night, &chrono_tz::Europe::London),
        gas: derive_gas(&gas, now),
    })
}

/// Whether the gas unit rate may be filled in: only readings known to be in
/// kWh can be priced, as the Octopus page does. The standing charge doesn't
/// depend on the unit.
pub fn gas_unit_rate_usable(settings: &crate::settings::Settings) -> bool {
    settings.octopus_gas_enabled && settings.octopus_gas_unit == "kwh"
}

/// The values a fill writes, to tell whether it changed anything.
type Written = (
    Option<TariffConfig>,
    Option<TariffConfig>,
    f64,
    f64,
    f64,
    f64,
    f64,
    crate::settings::OctopusTariffApplied,
);

fn written(s: &crate::settings::Settings) -> Written {
    let mut applied = s.octopus_tariff_applied.clone();
    applied.at = None;
    (
        s.import_tariff_config.clone(),
        s.export_tariff_config.clone(),
        s.import_tariff,
        s.export_tariff,
        s.import_standing_charge_p_per_day,
        s.gas_tariff_p_per_kwh,
        s.gas_standing_charge_p_per_day,
        applied,
    )
}

/// Write what could be derived into `settings`, leaving every part that
/// couldn't be exactly as it was (so Agile users keep their typed numbers),
/// and record which parts now follow Octopus. Returns whether anything
/// changed, so an unchanged sync doesn't rewrite the settings file.
///
/// The legacy flat `import_tariff` / `export_tariff` are kept in step, because
/// the daily report prices a single-window tariff with them.
pub fn apply_to_settings(
    settings: &mut crate::settings::Settings,
    derived: &Derivations,
    now: i64,
) -> bool {
    let before = written(settings);
    let mut applied = crate::settings::OctopusTariffApplied {
        at: settings.octopus_tariff_applied.at,
        ..Default::default()
    };

    if let Ok(t) = &derived.import {
        settings.import_tariff_config = Some(t.config.clone());
        if let Some(rate) = t.config.rate_for_minutes(12 * 60) {
            settings.import_tariff = rate;
        }
        if let Some(standing) = t.standing_charge_p_per_day {
            settings.import_standing_charge_p_per_day = standing.max(0.0);
        }
        applied.import = Some(t.tariff_code.clone());
    }
    if let Ok(t) = &derived.export {
        settings.export_tariff_config = Some(t.config.clone());
        if let Some(rate) = t.config.rate_for_minutes(12 * 60) {
            settings.export_tariff = rate;
        }
        applied.export = Some(t.tariff_code.clone());
    }
    if let Ok(g) = &derived.gas {
        if gas_unit_rate_usable(settings) {
            settings.gas_tariff_p_per_kwh = g.unit_rate_p_per_kwh;
        }
        if let Some(standing) = g.standing_charge_p_per_day {
            settings.gas_standing_charge_p_per_day = standing.max(0.0);
        }
        applied.gas = Some(g.tariff_code.clone());
    }
    settings.octopus_tariff_applied = applied;

    let changed = written(settings) != before;
    if changed {
        settings.octopus_tariff_applied.at = Some(now);
    }
    changed
}

/// The UK-local `HH:MM` of a Unix instant, for messages.
pub fn uk_clock(ts: i64, tz: &Tz) -> Option<String> {
    let local = chrono::DateTime::from_timestamp(ts, 0)?.with_timezone(tz);
    Some(hhmm((local.hour() * 60 + local.minute()) as u16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono_tz::Europe::London;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The instant `minute` minutes after UK-local midnight on `d`.
    fn at(d: NaiveDate, minute: i64) -> i64 {
        let start = London
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .timestamp();
        start + minute * 60
    }

    /// Monday 2026-11-09 12:00 UK time (GMT).
    fn now() -> i64 {
        at(date(2026, 11, 9), 12 * 60)
    }

    const E7: (u16, u16) = (30, 450);

    fn rec(
        point: &str,
        code: &str,
        rate_type: &str,
        from: i64,
        to: Option<i64>,
        pence: f64,
    ) -> RateRecord {
        RateRecord {
            meter_point: point.into(),
            tariff_code: code.into(),
            rate_type: rate_type.into(),
            valid_from: from,
            valid_to: to,
            pence,
        }
    }

    /// A rate that never changes, valid for all time.
    fn fixed(code: &str, pence: f64) -> Vec<RateRecord> {
        vec![rec("mp1", code, "standard", 0, None, pence)]
    }

    /// Daily periods `(start minute, end minute, pence)` as one record each,
    /// for every local day from `first` to `last` inclusive.
    fn daily(
        code: &str,
        rate_type: &str,
        first: NaiveDate,
        last: NaiveDate,
        periods: &[(i64, i64, f64)],
    ) -> Vec<RateRecord> {
        let mut out = Vec::new();
        let mut d = first;
        while d <= last {
            for &(start, end, pence) in periods {
                out.push(rec(
                    "mp1",
                    code,
                    rate_type,
                    at(d, start),
                    Some(at(d, end)),
                    pence,
                ));
            }
            d += Duration::days(1);
        }
        out
    }

    fn derive(records: &[RateRecord]) -> Result<DerivedTariff, NotDerivable> {
        derive_tariff(records, now(), E7, &London)
    }

    fn windows(t: &DerivedTariff) -> Vec<(String, String, f64)> {
        t.config
            .slots
            .iter()
            .map(|s| (s.start.clone(), s.end.clone(), s.rate))
            .collect()
    }

    fn w(start: &str, end: &str, rate: f64) -> (String, String, f64) {
        (start.into(), end.into(), rate)
    }

    // ---- products that fit ----

    #[test]
    fn a_fixed_rate_is_one_window_all_day() {
        let t = derive(&fixed("E-1R-FIX", 24.4545)).unwrap();
        assert_eq!(windows(&t), vec![w("00:00", "23:59", 0.244545)]);
        assert_eq!(t.tariff_code, "E-1R-FIX");
        assert_eq!(t.meter_point, "mp1");
    }

    #[test]
    fn go_style_off_peak_gives_three_windows() {
        let r = daily(
            "E-1R-GO",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &[(0, 30, 24.5), (30, 330, 7.5), (330, 1440, 24.5)],
        );
        let t = derive(&r).unwrap();
        assert_eq!(
            windows(&t),
            vec![
                w("00:00", "00:30", 0.245),
                w("00:30", "05:30", 0.075),
                w("05:30", "23:59", 0.245)
            ]
        );
    }

    #[test]
    fn an_off_peak_window_across_midnight_is_two_windows_either_side() {
        // 23:30–05:30 cheap.
        let r = daily(
            "E-1R-X",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &[(0, 330, 8.0), (330, 1410, 30.0), (1410, 1440, 8.0)],
        );
        let t = derive(&r).unwrap();
        assert_eq!(
            windows(&t),
            vec![
                w("00:00", "05:30", 0.08),
                w("05:30", "23:30", 0.30),
                w("23:30", "23:59", 0.08)
            ]
        );
        assert!(t.config.validate().is_ok());
    }

    #[test]
    fn cosy_style_three_cheap_periods_fit() {
        let r = daily(
            "E-1R-COSY",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &[
                (0, 240, 26.0),
                (240, 420, 13.0),
                (420, 780, 26.0),
                (780, 960, 13.0),
                (960, 1140, 39.0),
                (1140, 1320, 26.0),
                (1320, 1440, 13.0),
            ],
        );
        let t = derive(&r).unwrap();
        assert_eq!(t.config.slots.len(), 7);
        assert_eq!(
            t.config.slots[1],
            TariffSlot {
                start: "04:00".into(),
                end: "07:00".into(),
                rate: 0.13
            }
        );
        assert_eq!(t.config.slots[6].end, "23:59");
    }

    #[test]
    fn flux_style_night_day_and_peak_fit() {
        let r = daily(
            "E-1R-FLUX",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &[
                (0, 120, 28.0),
                (120, 300, 20.0),
                (300, 960, 28.0),
                (960, 1140, 40.0),
                (1140, 1440, 28.0),
            ],
        );
        assert_eq!(derive(&r).unwrap().config.slots.len(), 5);
    }

    #[test]
    fn economy_7_uses_the_night_window_to_split_day_and_night_rates() {
        let mut r = fixed_part("E-2R-E7", "day", 30.0);
        r.extend(fixed_part("E-2R-E7", "night", 12.0));
        let t = derive(&r).unwrap();
        assert_eq!(
            windows(&t),
            vec![
                w("00:00", "00:30", 0.30),
                w("00:30", "07:30", 0.12),
                w("07:30", "23:59", 0.30)
            ]
        );
    }

    fn fixed_part(code: &str, rate_type: &str, pence: f64) -> Vec<RateRecord> {
        vec![rec("mp1", code, rate_type, 0, None, pence)]
    }

    #[test]
    fn a_night_window_across_midnight_works_for_economy_7() {
        let mut r = fixed_part("E-2R-E7", "day", 30.0);
        r.extend(fixed_part("E-2R-E7", "night", 12.0));
        let t = derive_tariff(&r, now(), (23 * 60, 6 * 60), &London).unwrap();
        assert_eq!(
            windows(&t),
            vec![
                w("00:00", "06:00", 0.12),
                w("06:00", "23:00", 0.30),
                w("23:00", "23:59", 0.12)
            ]
        );
    }

    #[test]
    fn tracker_style_daily_value_changes_use_todays_rate() {
        // One window all day, a different price every day.
        let mut r = Vec::new();
        for (i, day) in (5..=12).enumerate() {
            let d = date(2026, 11, day);
            r.push(rec(
                "mp1",
                "E-1R-TRK",
                "standard",
                at(d, 0),
                Some(at(d + Duration::days(1), 0)),
                20.0 + i as f64,
            ));
        }
        let t = derive(&r).unwrap();
        // 2026-11-09 is the fifth record (index 4).
        assert_eq!(windows(&t), vec![w("00:00", "23:59", 0.24)]);
    }

    #[test]
    fn a_price_cap_change_mid_week_moves_the_values_not_the_shape() {
        // Same windows every day; yesterday's rates were lower than today's.
        let mut r = daily(
            "E-1R-VAR",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 8),
            &[(0, 330, 7.0), (330, 1440, 24.0)],
        );
        r.extend(daily(
            "E-1R-VAR",
            "standard",
            date(2026, 11, 9),
            date(2026, 11, 12),
            &[(0, 330, 8.0), (330, 1440, 26.0)],
        ));
        let t = derive(&r).unwrap();
        assert_eq!(
            windows(&t),
            vec![w("00:00", "05:30", 0.08), w("05:30", "23:59", 0.26)]
        );
    }

    #[test]
    fn rates_with_decimals_are_rounded_to_six_places_in_pounds() {
        let t = derive(&fixed("E-1R-X", 24.45454)).unwrap();
        assert_eq!(t.config.slots[0].rate, 0.244545);
    }

    // ---- products that don't ----

    #[test]
    fn agile_style_half_hourly_rates_are_not_derivable() {
        let periods: Vec<(i64, i64, f64)> = (0..48)
            .map(|i| (i * 30, i * 30 + 30, 10.0 + (i % 7) as f64))
            .collect();
        let r = daily(
            "E-1R-AGILE",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &periods,
        );
        match derive(&r) {
            Err(NotDerivable::TooManyWindows(n)) => assert!(n > MAX_DERIVED_WINDOWS),
            other => panic!("expected TooManyWindows, got {other:?}"),
        }
    }

    #[test]
    fn windows_that_differ_from_the_day_before_are_not_derivable() {
        // Yesterday's cheap window ended an hour later than today's.
        let mut r = daily(
            "E-1R-X",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 8),
            &[(0, 390, 7.0), (390, 1440, 24.0)],
        );
        r.extend(daily(
            "E-1R-X",
            "standard",
            date(2026, 11, 9),
            date(2026, 11, 12),
            &[(0, 330, 7.0), (330, 1440, 24.0)],
        ));
        assert_eq!(derive(&r), Err(NotDerivable::VariesDaily));
    }

    #[test]
    fn a_reason_is_given_for_every_refusal() {
        for e in [
            NotDerivable::NoCurrentAgreement,
            NotDerivable::Ambiguous,
            NotDerivable::NotEnoughData,
            NotDerivable::TooManyWindows(48),
            NotDerivable::VariesDaily,
            NotDerivable::Invalid("x".into()),
        ] {
            assert!(!e.reason().is_empty());
        }
        assert!(NotDerivable::TooManyWindows(48).reason().contains("48"));
    }

    // ---- agreements ----

    #[test]
    fn no_rates_covering_now_means_no_current_agreement() {
        let r = vec![rec(
            "mp1",
            "E-1R-OLD",
            "standard",
            0,
            Some(now() - 3600),
            20.0,
        )];
        assert_eq!(derive(&r), Err(NotDerivable::NoCurrentAgreement));
        assert_eq!(derive(&[]), Err(NotDerivable::NoCurrentAgreement));
    }

    #[test]
    fn two_current_agreements_are_ambiguous() {
        let mut r = fixed("E-1R-A", 20.0);
        r.extend(fixed("E-1R-B", 25.0));
        assert_eq!(derive(&r), Err(NotDerivable::Ambiguous));
        let mut two_meters = fixed("E-1R-A", 20.0);
        two_meters.push(rec("mp2", "E-1R-A", "standard", 0, None, 20.0));
        assert_eq!(derive(&two_meters), Err(NotDerivable::Ambiguous));
    }

    #[test]
    fn a_new_agreement_is_read_from_its_own_rates_only() {
        // The old tariff ended two days ago with different windows; it must
        // neither be read nor make the new one look unstable.
        let mut r = daily(
            "E-1R-OLD",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 6),
            &[(0, 600, 5.0), (600, 1440, 30.0)],
        );
        // Old agreement closes exactly when the new one starts.
        r.push(rec(
            "mp1",
            "E-1R-OLD",
            "standard",
            at(date(2026, 11, 7), 0),
            Some(at(date(2026, 11, 7), 0)),
            99.0,
        ));
        r.extend(daily(
            "E-1R-NEW",
            "standard",
            date(2026, 11, 7),
            date(2026, 11, 12),
            &[(0, 330, 7.0), (330, 1440, 24.0)],
        ));
        let t = derive(&r).unwrap();
        assert_eq!(t.tariff_code, "E-1R-NEW");
        assert_eq!(
            windows(&t),
            vec![w("00:00", "05:30", 0.07), w("05:30", "23:59", 0.24)]
        );
    }

    // ---- gaps and clock changes ----

    #[test]
    fn a_gap_in_todays_rates_falls_back_to_the_last_complete_day() {
        // Rates exist to yesterday and for the morning of today only.
        let mut r = daily(
            "E-1R-X",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 8),
            &[(0, 330, 7.0), (330, 1440, 24.0)],
        );
        r.push(rec(
            "mp1",
            "E-1R-X",
            "standard",
            at(date(2026, 11, 9), 0),
            Some(at(date(2026, 11, 9), 780)),
            7.0,
        ));
        let t = derive(&r).unwrap();
        // Today is incomplete, so yesterday's day is the pattern (and the
        // day before it must agree, which it does).
        assert_eq!(
            windows(&t),
            vec![w("00:00", "05:30", 0.07), w("05:30", "23:59", 0.24)]
        );
    }

    #[test]
    fn rates_for_no_complete_day_are_not_enough() {
        let r = vec![rec(
            "mp1",
            "E-1R-X",
            "standard",
            at(date(2026, 11, 9), 600),
            None,
            20.0,
        )];
        assert_eq!(derive(&r), Err(NotDerivable::NotEnoughData));
    }

    #[test]
    fn a_missing_night_rate_for_economy_7_is_not_enough() {
        let r = fixed_part("E-2R-E7", "day", 30.0);
        assert_eq!(derive(&r), Err(NotDerivable::NotEnoughData));
    }

    #[test]
    fn a_standing_charge_alone_is_not_a_tariff() {
        let r = fixed_part("E-1R-X", "standing", 50.0);
        assert_eq!(derive(&r), Err(NotDerivable::NotEnoughData));
    }

    #[test]
    fn a_clock_change_day_is_skipped_not_mistaken_for_a_gap() {
        // Today is Sunday 2026-10-25, the 25-hour day the clocks go back.
        let today = at(date(2026, 10, 25), 12 * 60);
        let r = daily(
            "E-1R-X",
            "standard",
            date(2026, 10, 15),
            date(2026, 10, 30),
            &[(0, 330, 7.0), (330, 1440, 24.0)],
        );
        let t = derive_tariff(&r, today, E7, &London).unwrap();
        assert_eq!(
            windows(&t),
            vec![w("00:00", "05:30", 0.07), w("05:30", "23:59", 0.24)]
        );
    }

    #[test]
    fn the_spring_forward_day_is_skipped_too() {
        // Sunday 2026-03-29 has 23 hours.
        let today = at(date(2026, 3, 29), 12 * 60);
        let r = daily(
            "E-1R-X",
            "standard",
            date(2026, 3, 18),
            date(2026, 4, 2),
            &[(0, 330, 7.0), (330, 1440, 24.0)],
        );
        let t = derive_tariff(&r, today, E7, &London).unwrap();
        assert_eq!(t.config.slots.len(), 2);
    }

    // ---- standing charge and the other direction ----

    #[test]
    fn the_standing_charge_is_the_one_covering_now_in_pence() {
        let mut r = fixed("E-1R-X", 24.0);
        r.push(rec("mp1", "E-1R-X", "standing", 0, Some(now() - 1), 40.0));
        r.push(rec("mp1", "E-1R-X", "standing", now() - 1, None, 54.86));
        assert_eq!(derive(&r).unwrap().standing_charge_p_per_day, Some(54.86));
    }

    #[test]
    fn no_standing_charge_record_leaves_it_unset() {
        assert_eq!(
            derive(&fixed("E-1R-X", 24.0))
                .unwrap()
                .standing_charge_p_per_day,
            None
        );
    }

    #[test]
    fn an_export_tariff_derives_the_same_way() {
        let t = derive(&fixed("E-1R-OUTGOING-FIX", 15.0)).unwrap();
        assert_eq!(windows(&t), vec![w("00:00", "23:59", 0.15)]);
    }

    #[test]
    fn invalid_supplier_rates_are_ignored() {
        let mut r = fixed("E-1R-X", 24.0);
        r.push(rec("mp1", "E-1R-X", "standard", 1, None, f64::NAN));
        r.push(rec("mp1", "E-1R-X", "standard", 2, None, -3.0));
        r.push(rec("mp1", "E-1R-X", "mystery", 3, None, 9.0));
        assert_eq!(
            windows(&derive(&r).unwrap()),
            vec![w("00:00", "23:59", 0.24)]
        );
    }

    #[test]
    fn every_derived_tariff_passes_the_settings_validation() {
        let r = daily(
            "E-1R-GO",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &[(0, 30, 24.5), (30, 330, 7.5), (330, 1440, 24.5)],
        );
        assert!(derive(&r).unwrap().config.validate().is_ok());
    }

    // ---- gas ----

    #[test]
    fn gas_takes_the_flat_rate_and_standing_charge_in_force() {
        let r = vec![
            rec("g1", "G-1R-X", "standard", 0, None, 6.1),
            rec("g1", "G-1R-X", "standing", 0, None, 29.4),
        ];
        let g = derive_gas(&r, now()).unwrap();
        assert_eq!(g.unit_rate_p_per_kwh, 6.1);
        assert_eq!(g.standing_charge_p_per_day, Some(29.4));
        assert_eq!(g.tariff_code, "G-1R-X");
    }

    #[test]
    fn gas_without_a_standing_charge_still_gives_the_rate() {
        let r = vec![rec("g1", "G-1R-X", "standard", 0, None, 6.1)];
        assert_eq!(
            derive_gas(&r, now()).unwrap().standing_charge_p_per_day,
            None
        );
    }

    #[test]
    fn gas_refusals_match_electricity() {
        assert_eq!(
            derive_gas(&[], now()),
            Err(NotDerivable::NoCurrentAgreement)
        );
        let two = vec![
            rec("g1", "G-A", "standard", 0, None, 6.0),
            rec("g1", "G-B", "standard", 0, None, 7.0),
        ];
        assert_eq!(derive_gas(&two, now()), Err(NotDerivable::Ambiguous));
        let standing_only = vec![rec("g1", "G-A", "standing", 0, None, 29.0)];
        assert_eq!(
            derive_gas(&standing_only, now()),
            Err(NotDerivable::NotEnoughData)
        );
    }

    #[test]
    fn the_uk_clock_follows_summer_time() {
        let june = at(date(2026, 6, 15), 0); // 00:00 BST = 23:00 UTC the day before
        assert_eq!(uk_clock(june, &London).as_deref(), Some("00:00"));
        assert_eq!(uk_clock(june + 90 * 60, &London).as_deref(), Some("01:30"));
    }

    // ---- the night window ----

    #[test]
    fn a_night_window_inside_the_day_is_start_inclusive_and_end_exclusive() {
        let w = (30, 450);
        assert!(!in_night_window(29, w));
        assert!(in_night_window(30, w));
        assert!(in_night_window(449, w));
        assert!(!in_night_window(450, w));
    }

    #[test]
    fn a_night_window_across_midnight_covers_both_ends_of_the_day() {
        let w = (23 * 60 + 30, 5 * 60 + 30);
        assert!(in_night_window(23 * 60 + 30, w));
        assert!(in_night_window(23 * 60 + 59, w));
        assert!(in_night_window(0, w));
        assert!(in_night_window(5 * 60 + 29, w));
        assert!(!in_night_window(5 * 60 + 30, w));
        assert!(!in_night_window(12 * 60, w));
        assert!(!in_night_window(23 * 60 + 29, w));
    }

    #[test]
    fn economy_7_hours_come_from_settings_with_the_usual_default_for_junk() {
        let mut s = crate::settings::Settings::default();
        assert_eq!(economy7_window(&s), (30, 450));
        s.octopus_economy7_start = "01:00".into();
        s.octopus_economy7_end = "08:15".into();
        assert_eq!(economy7_window(&s), (60, 495));
        s.octopus_economy7_start = "junk".into();
        s.octopus_economy7_end = "25:99".into();
        assert_eq!(economy7_window(&s), (30, 450));
    }

    // ---- day and night rates without published hours (Intelligent Octopus Go) ----

    const IOG_SMB: &str = "E-1R-IOG-SMB-VAR-24-10-29-H";

    #[test]
    fn intelligent_octopus_go_nights_run_2330_to_0530_whatever_the_economy_7_hours() {
        assert_eq!(night_window_for(IOG_SMB, E7), (23 * 60 + 30, 5 * 60 + 30));
        assert_eq!(
            night_window_for("E-1R-INTELLI-VAR-24-10-29-H", E7),
            (23 * 60 + 30, 5 * 60 + 30)
        );
        assert_eq!(
            night_window_for("E-1R-IOG-BB-VAR-23-08-31-H", (1, 2)),
            (1410, 330)
        );
    }

    #[test]
    fn other_tariffs_keep_the_economy_7_hours() {
        assert_eq!(night_window_for("E-2R-VAR-22-11-01-H", E7), E7);
        assert_eq!(night_window_for("E-1R-GO-VAR-22-03-29-H", E7), E7);
        assert_eq!(night_window_for("tariff-electricity_import", E7), E7);
    }

    #[test]
    fn intelligent_go_day_and_night_rates_make_three_windows_at_the_go_hours() {
        let mut r = fixed_part(IOG_SMB, "day", 36.3918);
        r.extend(fixed_part(IOG_SMB, "night", 6.6));
        r.push(rec("mp1", IOG_SMB, "standing", 0, None, 47.4715));
        let t = derive(&r).unwrap();
        assert_eq!(
            windows(&t),
            vec![
                w("00:00", "05:30", 0.066),
                w("05:30", "23:30", 0.363918),
                w("23:30", "23:59", 0.066)
            ]
        );
        assert_eq!(t.standing_charge_p_per_day, Some(47.4715));
    }

    #[test]
    fn economy_7_codes_still_use_the_households_own_hours() {
        let mut r = fixed_part("E-2R-VAR-22-11-01-H", "day", 30.0);
        r.extend(fixed_part("E-2R-VAR-22-11-01-H", "night", 12.0));
        let t = derive_tariff(&r, now(), (60, 480), &London).unwrap();
        assert_eq!(
            windows(&t),
            vec![
                w("00:00", "01:00", 0.30),
                w("01:00", "08:00", 0.12),
                w("08:00", "23:59", 0.30)
            ]
        );
    }

    // ---- applying to settings ----

    fn derivations(
        import: Result<DerivedTariff, NotDerivable>,
        export: Result<DerivedTariff, NotDerivable>,
        gas: Result<DerivedGas, NotDerivable>,
    ) -> Derivations {
        Derivations {
            import,
            export,
            gas,
        }
    }

    fn go_tariff() -> DerivedTariff {
        let r = daily(
            "E-1R-GO",
            "standard",
            date(2026, 11, 1),
            date(2026, 11, 12),
            &[(0, 30, 24.5), (30, 330, 7.5), (330, 1440, 24.5)],
        );
        let mut t = derive(&r).unwrap();
        t.standing_charge_p_per_day = Some(54.86);
        t
    }

    fn gas() -> DerivedGas {
        DerivedGas {
            meter_point: "g1".into(),
            tariff_code: "G-1R-X".into(),
            unit_rate_p_per_kwh: 6.1,
            standing_charge_p_per_day: Some(29.4),
        }
    }

    fn kwh_gas_settings() -> crate::settings::Settings {
        crate::settings::Settings {
            octopus_gas_enabled: true,
            octopus_gas_unit: "kwh".into(),
            ..crate::settings::Settings::default()
        }
    }

    #[test]
    fn applying_writes_the_windows_standing_charge_and_marks_the_parts_followed() {
        let mut s = kwh_gas_settings();
        let flat_export = derive(&fixed("E-1R-OUT", 15.0)).unwrap();
        let d = derivations(Ok(go_tariff()), Ok(flat_export.clone()), Ok(gas()));
        assert!(apply_to_settings(&mut s, &d, 1000));
        assert_eq!(s.import_tariff_config, Some(go_tariff().config));
        assert_eq!(s.export_tariff_config, Some(flat_export.config));
        assert_eq!(s.import_standing_charge_p_per_day, 54.86);
        assert_eq!(s.gas_tariff_p_per_kwh, 6.1);
        assert_eq!(s.gas_standing_charge_p_per_day, 29.4);
        assert_eq!(s.octopus_tariff_applied.import.as_deref(), Some("E-1R-GO"));
        assert_eq!(s.octopus_tariff_applied.export.as_deref(), Some("E-1R-OUT"));
        assert_eq!(s.octopus_tariff_applied.gas.as_deref(), Some("G-1R-X"));
        assert_eq!(s.octopus_tariff_applied.at, Some(1000));
    }

    #[test]
    fn the_legacy_flat_rates_follow_so_the_daily_report_agrees() {
        let mut s = kwh_gas_settings();
        let flat_import = derive(&fixed("E-1R-FIX", 24.4545)).unwrap();
        let flat_export = derive(&fixed("E-1R-OUT", 15.0)).unwrap();
        let d = derivations(
            Ok(flat_import),
            Ok(flat_export),
            Err(NotDerivable::NoCurrentAgreement),
        );
        apply_to_settings(&mut s, &d, 1);
        assert_eq!(s.import_tariff, 0.244545);
        assert_eq!(s.export_tariff, 0.15);
    }

    #[test]
    fn a_part_that_cannot_be_derived_keeps_what_the_user_typed() {
        let mut s = kwh_gas_settings();
        let typed = TariffConfig::flat(0.31);
        s.import_tariff_config = Some(typed.clone());
        s.import_standing_charge_p_per_day = 45.0;
        let d = derivations(
            Err(NotDerivable::TooManyWindows(48)),
            Ok(derive(&fixed("E-1R-OUT", 15.0)).unwrap()),
            Err(NotDerivable::NoCurrentAgreement),
        );
        assert!(apply_to_settings(&mut s, &d, 1));
        assert_eq!(s.import_tariff_config, Some(typed));
        assert_eq!(s.import_standing_charge_p_per_day, 45.0);
        assert_eq!(
            s.octopus_tariff_applied.import, None,
            "import stays editable"
        );
        assert_eq!(s.octopus_tariff_applied.export.as_deref(), Some("E-1R-OUT"));
    }

    #[test]
    fn applying_the_same_rates_again_changes_nothing_and_keeps_the_timestamp() {
        let mut s = kwh_gas_settings();
        let d = derivations(
            Ok(go_tariff()),
            Err(NotDerivable::NoCurrentAgreement),
            Ok(gas()),
        );
        assert!(apply_to_settings(&mut s, &d, 1000));
        let after_first = s.octopus_tariff_applied.clone();
        assert!(!apply_to_settings(&mut s, &d, 2000), "no churn");
        assert_eq!(s.octopus_tariff_applied, after_first);
        assert_eq!(s.octopus_tariff_applied.at, Some(1000));
    }

    #[test]
    fn a_changed_rate_is_applied_and_restamped() {
        let mut s = kwh_gas_settings();
        let d = derivations(
            Ok(go_tariff()),
            Err(NotDerivable::NoCurrentAgreement),
            Err(NotDerivable::NoCurrentAgreement),
        );
        apply_to_settings(&mut s, &d, 1000);
        let mut cheaper = go_tariff();
        cheaper.config.slots[1].rate = 0.05;
        let d = derivations(
            Ok(cheaper.clone()),
            Err(NotDerivable::NoCurrentAgreement),
            Err(NotDerivable::NoCurrentAgreement),
        );
        assert!(apply_to_settings(&mut s, &d, 3000));
        assert_eq!(s.import_tariff_config, Some(cheaper.config));
        assert_eq!(s.octopus_tariff_applied.at, Some(3000));
    }

    #[test]
    fn a_part_that_stops_being_derivable_is_released_but_its_values_stay() {
        let mut s = kwh_gas_settings();
        apply_to_settings(
            &mut s,
            &derivations(
                Ok(go_tariff()),
                Err(NotDerivable::NoCurrentAgreement),
                Err(NotDerivable::NoCurrentAgreement),
            ),
            1000,
        );
        let kept = s.import_tariff_config.clone();
        // The household moves to Agile: import is no longer derivable.
        apply_to_settings(
            &mut s,
            &derivations(
                Err(NotDerivable::TooManyWindows(48)),
                Err(NotDerivable::NoCurrentAgreement),
                Err(NotDerivable::NoCurrentAgreement),
            ),
            2000,
        );
        assert_eq!(
            s.octopus_tariff_applied.import, None,
            "unlocked for hand entry"
        );
        assert_eq!(
            s.import_tariff_config, kept,
            "the last values stay as the starting point"
        );
    }

    #[test]
    fn gas_in_cubic_metres_gets_the_standing_charge_but_no_unit_rate() {
        let mut s = crate::settings::Settings {
            octopus_gas_enabled: true,
            octopus_gas_unit: "m3".into(),
            ..crate::settings::Settings::default()
        };
        s.gas_tariff_p_per_kwh = 0.0;
        let d = derivations(
            Err(NotDerivable::NoCurrentAgreement),
            Err(NotDerivable::NoCurrentAgreement),
            Ok(gas()),
        );
        apply_to_settings(&mut s, &d, 1);
        assert_eq!(s.gas_tariff_p_per_kwh, 0.0);
        assert_eq!(s.gas_standing_charge_p_per_day, 29.4);
        assert!(!gas_unit_rate_usable(&s));
    }

    #[test]
    fn nothing_derivable_changes_nothing() {
        let mut s = crate::settings::Settings::default();
        let before = s.import_tariff;
        let d = derivations(
            Err(NotDerivable::NoCurrentAgreement),
            Err(NotDerivable::NoCurrentAgreement),
            Err(NotDerivable::NoCurrentAgreement),
        );
        assert!(!d.any());
        assert!(!apply_to_settings(&mut s, &d, 1));
        assert_eq!(s.import_tariff, before);
        assert_eq!(s.octopus_tariff_applied.at, None);
    }

    #[test]
    fn a_negative_standing_charge_is_written_as_zero() {
        let mut s = kwh_gas_settings();
        let mut t = go_tariff();
        t.standing_charge_p_per_day = Some(-5.0);
        apply_to_settings(
            &mut s,
            &derivations(
                Ok(t),
                Err(NotDerivable::NoCurrentAgreement),
                Err(NotDerivable::NoCurrentAgreement),
            ),
            1,
        );
        assert_eq!(s.import_standing_charge_p_per_day, 0.0);
    }

    #[test]
    fn a_standing_charge_the_supplier_does_not_list_leaves_the_typed_one() {
        let mut s = kwh_gas_settings();
        s.import_standing_charge_p_per_day = 45.0;
        let mut t = go_tariff();
        t.standing_charge_p_per_day = None;
        apply_to_settings(
            &mut s,
            &derivations(
                Ok(t),
                Err(NotDerivable::NoCurrentAgreement),
                Err(NotDerivable::NoCurrentAgreement),
            ),
            1,
        );
        assert_eq!(s.import_standing_charge_p_per_day, 45.0);
    }

    // ---- loading records ----

    #[test]
    fn records_load_from_the_database_by_meter_kind_and_expiry() {
        use crate::history::{HistoryDb, OctopusTariffPriceRow};
        let dir = std::env::temp_dir().join(format!("givenergy-octotariff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = HistoryDb::open(&dir.join("history.db")).unwrap();
        let row = |kind: &str, to: Option<i64>, pence: f64| OctopusTariffPriceRow {
            meter_kind: kind.into(),
            meter_point: "mp1".into(),
            tariff_code: "E-1R-X".into(),
            valid_from: 100 + pence as i64,
            valid_to: to,
            value_inc_vat: pence,
            rate_type: "standard".into(),
        };
        db.upsert_octopus_tariff_prices(&[
            row("electricity_import", None, 20.0),
            row("electricity_import", Some(5_000), 21.0), // ended long ago
            row("electricity_import", Some(now() + 10), 22.0),
            row("electricity_export", None, 15.0),
        ])
        .unwrap();
        let got = db
            .octopus_rate_records("electricity_import", now() - 86_400)
            .unwrap();
        let pences: Vec<f64> = got.iter().map(|r| r.pence).collect();
        assert_eq!(pences, vec![20.0, 22.0]);
        assert!(db.octopus_rate_records("gas", 0).unwrap().is_empty());
    }
}

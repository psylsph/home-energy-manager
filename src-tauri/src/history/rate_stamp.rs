//! The tariff in force when a history row was recorded (issue #455).
//!
//! History costs used to price every past minute with the tariff as it is
//! *now*, so editing a rate silently rewrote last month. Each `readings` row
//! now carries the import rate, export rate and standing charge that applied
//! when it was written, and the cost readers price from the row.
//!
//! `None` means "not recorded" (rows from before this feature, or a rate that
//! couldn't be resolved) and is stored as SQL `NULL`; `Some(0.0)` is a real
//! rate (free export). The two must never be conflated.

use crate::settings::{Settings, TariffConfig};

/// The rates that applied at one history row's minute.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RateStamp {
    /// Import unit rate, £/kWh.
    pub import_rate: Option<f64>,
    /// Export unit rate, £/kWh.
    pub export_rate: Option<f64>,
    /// Import standing charge, pence/day.
    pub standing_charge_p_per_day: Option<f64>,
}

impl RateStamp {
    /// The rates in force at `timestamp` (Unix seconds) under `settings`, with
    /// the minute of day taken in the host's local zone, as the cost readers
    /// do.
    pub fn resolve(settings: &Settings, timestamp: i64) -> Self {
        Self::resolve_in(settings, timestamp, &chrono::Local)
    }

    /// [`Self::resolve`] in an explicit zone, so tests are deterministic.
    ///
    /// Resolves the way the History API prices a reading: the structured
    /// config when there is one, otherwise a flat config built from the
    /// legacy scalar rate. A side that can't be resolved is left `None`; the
    /// others are still stamped.
    pub(crate) fn resolve_in<TZ: chrono::TimeZone>(
        settings: &Settings,
        timestamp: i64,
        tz: &TZ,
    ) -> Self {
        let minute = super::local_minutes_of_day(timestamp, tz);
        Self {
            import_rate: rate_at(
                settings.import_tariff_config.as_ref(),
                settings.import_tariff,
                minute,
            ),
            export_rate: rate_at(
                settings.export_tariff_config.as_ref(),
                settings.export_tariff,
                minute,
            ),
            // Negative clamps to zero, as the cost readers treat it.
            standing_charge_p_per_day: Some(settings.import_standing_charge_p_per_day)
                .filter(|p| p.is_finite())
                .map(|p| p.max(0.0)),
        }
    }
}

/// The £/kWh rate of `config` (or of a flat config built from `legacy_flat`
/// when there is none) at `minute` of the day, or `None` when it isn't a usable
/// rate: no slots, or a rate that is not a finite, non-negative number.
fn rate_at(config: Option<&TariffConfig>, legacy_flat: f64, minute: u16) -> Option<f64> {
    let rate = match config {
        Some(config) => config.rate_for_minutes(minute),
        None => Some(legacy_flat),
    };
    rate.filter(|r| r.is_finite() && *r >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::TariffSlot;

    /// 2026-10-21 12:00:00 UTC.
    const NOON: i64 = 1_792_584_000;

    fn slot(start: &str, end: &str, rate: f64) -> TariffSlot {
        TariffSlot {
            start: start.into(),
            end: end.into(),
            rate,
        }
    }

    /// 0.30 £/kWh import with a 02:00–05:00 cheap window at 0.07; export 0.15.
    fn settings() -> Settings {
        Settings {
            import_tariff: 0.30,
            export_tariff: 0.15,
            import_standing_charge_p_per_day: 54.86,
            import_tariff_config: Some(TariffConfig {
                slots: vec![
                    slot("00:00", "02:00", 0.30),
                    slot("02:00", "05:00", 0.07),
                    slot("05:00", "23:59", 0.30),
                ],
            }),
            ..Settings::default()
        }
    }

    fn utc() -> chrono::Utc {
        chrono::Utc
    }

    #[test]
    fn a_stamp_carries_the_rate_of_the_minute_it_is_taken_in() {
        let s = settings();
        let cheap = RateStamp::resolve_in(&s, NOON - 10 * 3600 + 30 * 60, &utc()); // 02:30
        assert_eq!(cheap.import_rate, Some(0.07));
        let day = RateStamp::resolve_in(&s, NOON, &utc());
        assert_eq!(day.import_rate, Some(0.30));
    }

    #[test]
    fn the_cheap_window_boundaries_are_half_open_like_the_cost_readers() {
        let s = settings();
        let at = |h: i64, m: i64| NOON - 12 * 3600 + h * 3600 + m * 60;
        assert_eq!(
            RateStamp::resolve_in(&s, at(1, 59), &utc()).import_rate,
            Some(0.30)
        );
        assert_eq!(
            RateStamp::resolve_in(&s, at(2, 0), &utc()).import_rate,
            Some(0.07)
        );
        assert_eq!(
            RateStamp::resolve_in(&s, at(4, 59), &utc()).import_rate,
            Some(0.07)
        );
        assert_eq!(
            RateStamp::resolve_in(&s, at(5, 0), &utc()).import_rate,
            Some(0.30)
        );
        assert_eq!(
            RateStamp::resolve_in(&s, at(23, 59), &utc()).import_rate,
            Some(0.30),
            "the last minute of the day belongs to the final window"
        );
    }

    #[test]
    fn the_minute_of_day_is_taken_in_the_given_zone() {
        // 12:00 UTC is 13:00 at +01:00, still the 0.30 window; but 01:30 UTC
        // is 02:30 there, inside the cheap window.
        let s = settings();
        let plus_one = chrono::FixedOffset::east_opt(3600).unwrap();
        let early = NOON - 10 * 3600 - 30 * 60; // 01:30 UTC
        assert_eq!(
            RateStamp::resolve_in(&s, early, &utc()).import_rate,
            Some(0.30)
        );
        assert_eq!(
            RateStamp::resolve_in(&s, early, &plus_one).import_rate,
            Some(0.07)
        );
    }

    #[test]
    fn the_legacy_flat_rates_are_used_when_there_is_no_structured_config() {
        let s = Settings {
            import_tariff: 0.285,
            export_tariff: 0.12,
            import_tariff_config: None,
            export_tariff_config: None,
            ..Settings::default()
        };
        let stamp = RateStamp::resolve_in(&s, NOON, &utc());
        assert_eq!(stamp.import_rate, Some(0.285));
        assert_eq!(stamp.export_rate, Some(0.12));
    }

    #[test]
    fn the_export_rate_follows_its_own_time_of_use_config() {
        let mut s = settings();
        s.export_tariff_config = Some(TariffConfig {
            slots: vec![
                slot("00:00", "16:00", 0.10),
                slot("16:00", "19:00", 0.30),
                slot("19:00", "23:59", 0.10),
            ],
        });
        let at = |h: i64| NOON - 12 * 3600 + h * 3600;
        assert_eq!(
            RateStamp::resolve_in(&s, at(10), &utc()).export_rate,
            Some(0.10)
        );
        assert_eq!(
            RateStamp::resolve_in(&s, at(17), &utc()).export_rate,
            Some(0.30)
        );
    }

    #[test]
    fn a_free_export_rate_is_a_real_zero_not_an_unknown() {
        let s = Settings {
            export_tariff: 0.0,
            export_tariff_config: None,
            ..settings()
        };
        let stamp = RateStamp::resolve_in(&s, NOON, &utc());
        assert_eq!(stamp.export_rate, Some(0.0));
    }

    #[test]
    fn the_standing_charge_is_stamped_in_pence_and_zero_is_a_real_value() {
        let stamp = RateStamp::resolve_in(&settings(), NOON, &utc());
        assert_eq!(stamp.standing_charge_p_per_day, Some(54.86));
        let none = Settings {
            import_standing_charge_p_per_day: 0.0,
            ..settings()
        };
        assert_eq!(
            RateStamp::resolve_in(&none, NOON, &utc()).standing_charge_p_per_day,
            Some(0.0)
        );
    }

    #[test]
    fn a_negative_standing_charge_is_stamped_as_zero_like_the_cost_readers_treat_it() {
        let s = Settings {
            import_standing_charge_p_per_day: -10.0,
            ..settings()
        };
        assert_eq!(
            RateStamp::resolve_in(&s, NOON, &utc()).standing_charge_p_per_day,
            Some(0.0)
        );
    }

    #[test]
    fn a_rate_that_cannot_be_resolved_is_left_unrecorded_rather_than_guessed() {
        // An empty config has no rate for any minute; a non-finite scalar and
        // a non-finite standing charge are not rates either. Each side fails
        // on its own: the others are still stamped.
        let s = Settings {
            import_tariff_config: Some(TariffConfig { slots: vec![] }),
            export_tariff: f64::NAN,
            export_tariff_config: None,
            import_standing_charge_p_per_day: f64::INFINITY,
            ..Settings::default()
        };
        let stamp = RateStamp::resolve_in(&s, NOON, &utc());
        assert_eq!(stamp, RateStamp::default());

        let only_export_bad = Settings {
            export_tariff: f64::NAN,
            export_tariff_config: None,
            ..settings()
        };
        let stamp = RateStamp::resolve_in(&only_export_bad, NOON, &utc());
        assert_eq!(stamp.import_rate, Some(0.30));
        assert_eq!(stamp.export_rate, None);
        assert_eq!(stamp.standing_charge_p_per_day, Some(54.86));
    }
}

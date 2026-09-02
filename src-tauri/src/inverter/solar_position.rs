//! Offline solar-position calculation used to identify periods where genuine
//! PV generation is physically implausible.

use std::f64::consts::PI;

use chrono::{DateTime, Datelike, Timelike, Utc};

/// Below this solar elevation, low inverter-PV readings are treated as noise.
///
/// This is deliberately a PV-dark threshold rather than literal astronomical
/// night (-18 degrees): by -8 degrees there is no useful irradiance for a
/// domestic array, while the 50 W power ceiling still protects genuine output
/// around dawn and dusk.
pub(crate) const PV_DARK_ELEVATION_DEG: f64 = -8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SolarPosition {
    pub(crate) elevation_deg: f64,
    /// Solar hour angle in degrees: negative before solar noon, positive after.
    pub(crate) hour_angle_deg: f64,
}

impl SolarPosition {
    pub(crate) fn is_pv_dark(self) -> bool {
        self.elevation_deg <= PV_DARK_ELEVATION_DEG
    }

    /// True during the dark portion of the current solar day before sunrise.
    /// This distinguishes a post-midnight stale daily counter from the valid
    /// total that must remain visible after sunset but before midnight.
    pub(crate) fn is_pre_dawn_dark(self) -> bool {
        self.is_pv_dark() && self.hour_angle_deg < 0.0
    }
}

/// Calculate solar elevation and hour angle from UTC time and site coordinates.
///
/// Returns `None` for invalid/non-finite coordinates. The implementation is
/// intentionally offline: persisted Meteo coordinates are sufficient and no
/// weather request is required during polling.
pub(crate) fn calculate_solar_position(
    at: DateTime<Utc>,
    latitude_deg: f64,
    longitude_deg: f64,
) -> Option<SolarPosition> {
    if !latitude_deg.is_finite()
        || !longitude_deg.is_finite()
        || !(-90.0..=90.0).contains(&latitude_deg)
        || !(-180.0..=180.0).contains(&longitude_deg)
    {
        return None;
    }

    // NOAA's fractional-year approximation. Its sub-degree accuracy is far
    // tighter than the margin between the -8 degree PV-dark boundary and any
    // useful domestic-array output, and it needs neither a network request nor
    // a timezone database because both the timestamp and longitude are UTC.
    let fractional_hour = at.hour() as f64
        + at.minute() as f64 / 60.0
        + at.second() as f64 / 3600.0
        + at.nanosecond() as f64 / 3_600_000_000_000.0;
    let gamma = 2.0 * PI / 365.0 * (at.ordinal() as f64 - 1.0 + (fractional_hour - 12.0) / 24.0);

    let equation_of_time_minutes = 229.18
        * (0.000_075 + 0.001_868 * gamma.cos()
            - 0.032_077 * gamma.sin()
            - 0.014_615 * (2.0 * gamma).cos()
            - 0.040_849 * (2.0 * gamma).sin());
    let declination_rad = 0.006_918 - 0.399_912 * gamma.cos() + 0.070_257 * gamma.sin()
        - 0.006_758 * (2.0 * gamma).cos()
        + 0.000_907 * (2.0 * gamma).sin()
        - 0.002_697 * (3.0 * gamma).cos()
        + 0.001_48 * (3.0 * gamma).sin();

    let utc_minutes = fractional_hour * 60.0;
    let true_solar_minutes =
        (utc_minutes + equation_of_time_minutes + 4.0 * longitude_deg).rem_euclid(1440.0);
    let hour_angle_deg = true_solar_minutes / 4.0 - 180.0;
    let latitude_rad = latitude_deg.to_radians();
    let hour_angle_rad = hour_angle_deg.to_radians();
    let sin_elevation = latitude_rad.sin() * declination_rad.sin()
        + latitude_rad.cos() * declination_rad.cos() * hour_angle_rad.cos();
    let elevation_deg = sin_elevation.clamp(-1.0, 1.0).asin().to_degrees();

    Some(SolarPosition {
        elevation_deg,
        hour_angle_deg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn greenwich_midnight_is_pv_dark_and_pre_dawn() {
        let at = Utc.with_ymd_and_hms(2026, 9, 2, 0, 50, 0).unwrap();
        let position = calculate_solar_position(at, 51.48, 0.0).unwrap();

        assert!(position.elevation_deg < PV_DARK_ELEVATION_DEG);
        assert!(position.is_pre_dawn_dark());
    }

    #[test]
    fn greenwich_summer_noon_is_daylight() {
        let at = Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap();
        let position = calculate_solar_position(at, 51.48, 0.0).unwrap();

        assert!(position.elevation_deg > 60.0);
        assert!(!position.is_pv_dark());
    }

    #[test]
    fn invalid_coordinates_disable_solar_position_filtering() {
        let at = Utc.with_ymd_and_hms(2026, 9, 2, 0, 50, 0).unwrap();

        assert_eq!(calculate_solar_position(at, 91.0, 0.0), None);
        assert_eq!(calculate_solar_position(at, 51.48, 181.0), None);
        assert_eq!(calculate_solar_position(at, f64::NAN, 0.0), None);
    }
}

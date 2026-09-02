//! Offline solar-position calculation used to identify periods where genuine
//! PV generation is physically implausible.

use chrono::{DateTime, Utc};

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
    _at: DateTime<Utc>,
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

    // RED seam: the NOAA calculation is supplied by the GREEN commit.
    Some(SolarPosition {
        elevation_deg: 0.0,
        hour_angle_deg: 0.0,
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

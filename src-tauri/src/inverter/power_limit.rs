//! Conversion between a charge/discharge power-limit percentage, the raw
//! register value that stores it, and the power it allows.
//!
//! The Control page sliders (and Adaptive Charge, and the forecast) always
//! speak in "percent of the inverter's maximum battery power". The registers
//! do not all store that:
//!
//! - **Direct banks** (`AcBank` HR 313/314, `ThreePhase` HR 1110/1108) hold
//!   1–100 as a percentage of the inverter's maximum (GivTCP `read.py`:
//!   `batmaxrate * limit / 100`).
//! - **`HalfScale`** (single-phase DC hybrids, HR 111/112, 0–50) holds a
//!   percentage of battery *capacity*, with 50 meaning 0.5C. GivTCP
//!   `write.py` stores `watts / (capacity / 2) * 50` and `read.py` reads
//!   `min(reg / 100 * capacity_w, inverter_max)`. Writing "62% of the maximum"
//!   as register 31 therefore asked a 9.5 kWh pack for 2945 W, above a 2.6 kW
//!   inverter, and limited nothing (issue #346).
//!
//! The same arithmetic lives in `src/lib/powerLimit.ts`; keep them in step.
//!
//! When the maximum or capacity is unknown there is nothing to scale by and the
//! half-scale register falls back to a plain doubling.

use crate::inverter::model::{InverterSnapshot, PowerLimitBank};

/// Largest value the half-scale register can hold.
const HALF_SCALE_MAX_RAW: u16 = 50;

/// Plausible range for a pack's capacity in kWh. The smallest GivEnergy battery
/// is 2.6 kWh and the largest supported systems are a few tens of kWh. HR 55 is
/// a raw u16 with no sanitiser on single-phase inverters, so one corrupt dongle
/// read can report thousands of kWh; acting on that would collapse the
/// conversion ratio and write register 0 or 1 for a 40% request, so a capacity
/// outside this range is treated as unknown. Keep in step with
/// `MIN/MAX_PLAUSIBLE_CAPACITY_KWH` in `src/lib/powerLimit.ts`.
const MIN_PLAUSIBLE_CAPACITY_KWH: f32 = 1.0;
const MAX_PLAUSIBLE_CAPACITY_KWH: f32 = 150.0;

/// What a charge/discharge limit register stores for one inverter: the register
/// bank plus the inverter's maximum battery power and the pack size, which the
/// half-scale conversion needs. Build it once from the snapshot and use it for
/// every conversion so no call site wires those inputs up on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerLimitScale {
    pub bank: PowerLimitBank,
    /// The inverter's maximum battery power in watts; 0 when unknown.
    pub max_battery_power_w: u32,
    /// Total battery capacity in kWh; 0 when unknown.
    pub capacity_kwh: f32,
}

impl PowerLimitScale {
    pub fn new(bank: PowerLimitBank, max_battery_power_w: u32, capacity_kwh: f32) -> Self {
        Self {
            bank,
            max_battery_power_w,
            capacity_kwh,
        }
    }

    pub fn from_snapshot(snapshot: &InverterSnapshot) -> Self {
        Self::new(
            snapshot.device_type.power_limit_bank(),
            snapshot.max_battery_power_w,
            snapshot.battery_capacity_kwh,
        )
    }

    /// Whether the scale has the inputs the half-scale conversion needs: a known
    /// maximum and a plausible pack size. Without them the half-scale register
    /// falls back to a plain doubling, which limits nothing on a large pack.
    pub fn is_known(&self) -> bool {
        self.max_battery_power_w > 0
            && (MIN_PLAUSIBLE_CAPACITY_KWH..=MAX_PLAUSIBLE_CAPACITY_KWH)
                .contains(&self.capacity_kwh)
    }

    /// Whether `percent` can be converted faithfully. A partial percentage on the
    /// half-scale register depends on the pack size, and without it the only
    /// option is the plain doubling that limits nothing on a large pack, so
    /// callers must refuse or hold off rather than write it. 0% and 100%
    /// (register 0 and 50) never need the pack size, and direct banks are plain
    /// percentages.
    pub fn can_convert(&self, percent: u16) -> bool {
        self.bank.is_direct() || !(1..100).contains(&percent) || self.is_known()
    }

    /// Half-scale register units per percent of the inverter's maximum.
    ///
    /// One register unit is 1% of battery capacity, so 1% of the inverter's
    /// maximum is `max_w / capacity_w` units. Register 50 is 0.5C however the
    /// maximum is stated, so the maximum is capped at `capacity / 2` (as the
    /// decoder does). Falls back to 0.5, the plain half scale, when either
    /// input is unknown.
    fn raw_per_percent(&self) -> f64 {
        if self.is_known() {
            let capacity_w = f64::from(self.capacity_kwh) * 1000.0;
            f64::from(self.max_battery_power_w).min(capacity_w / 2.0) / capacity_w
        } else {
            f64::from(HALF_SCALE_MAX_RAW) / 100.0
        }
    }

    /// The register value that requests `percent` (0–100) of the inverter's
    /// maximum battery power, rounded to the nearest register step.
    ///
    /// 100% on the half scale writes 50 (0.5C), the register maximum and
    /// factory default: "no limit". It is above any inverter's rating and,
    /// unlike a value derived from the pack size, cannot throttle if the
    /// capacity is misread.
    pub fn percent_to_raw(&self, percent: u16) -> u16 {
        let percent = percent.min(100);
        if self.bank.is_direct() {
            return percent;
        }
        if percent >= 100 {
            return HALF_SCALE_MAX_RAW;
        }
        let exact = f64::from(percent) * self.raw_per_percent();
        // A non-zero percentage must never round down to register 0, which
        // stops charging (and which the forecast reads as "unset"); only a
        // deliberate 0% writes 0.
        let floor = u16::from(percent > 0);
        (exact.round() as u16).clamp(floor, HALF_SCALE_MAX_RAW)
    }

    /// The percentage of the inverter's maximum a register value allows.
    /// Anything at or above the value that reaches the maximum (including the
    /// factory default of 50) reads as 100%.
    pub fn raw_to_percent(&self, raw: u16) -> u16 {
        if self.bank.is_direct() {
            return raw.min(100);
        }
        let raw = raw.min(HALF_SCALE_MAX_RAW);
        let percent = f64::from(raw) / self.raw_per_percent();
        (percent.round() as u16).min(100)
    }

    /// The battery power (W) a register value allows: what the forecast should
    /// assume the inverter can charge or discharge at. Zero when the inverter's
    /// maximum is unknown.
    pub fn raw_to_watts(&self, raw: u16) -> f64 {
        if self.max_battery_power_w == 0 {
            return 0.0;
        }
        let max_w = f64::from(self.max_battery_power_w);
        if !self.bank.is_direct() && self.is_known() {
            let capacity_w = f64::from(self.capacity_kwh) * 1000.0;
            // GivTCP read.py: min(reg / 100 * capacity_w, inverter_max).
            let requested = f64::from(raw.min(HALF_SCALE_MAX_RAW)) / 100.0 * capacity_w;
            return requested.min(max_w);
        }
        f64::from(self.raw_to_percent(raw)) / 100.0 * max_w
    }
}

/// A requested charge rate: a share of the inverter's maximum battery power
/// plus the scale needed to turn it into a register value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChargeRateRequest {
    /// 0–100, a percentage of the inverter's maximum battery power.
    pub percent: u16,
    pub scale: PowerLimitScale,
}

impl ChargeRateRequest {
    pub fn for_snapshot(percent: u16, snapshot: &InverterSnapshot) -> Self {
        Self {
            percent,
            scale: PowerLimitScale::from_snapshot(snapshot),
        }
    }

    /// Whether this request cannot be converted faithfully; see
    /// [`PowerLimitScale::can_convert`].
    pub fn needs_pack_size(&self) -> bool {
        !self.scale.can_convert(self.percent)
    }

    /// The register value that delivers this request.
    pub fn to_raw(self) -> u16 {
        self.scale.percent_to_raw(self.percent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Thin shims so the conversions read as plain functions of (bank, inputs).
    fn percent_to_raw(bank: PowerLimitBank, percent: u16, max_w: u32, capacity_kwh: f32) -> u16 {
        PowerLimitScale::new(bank, max_w, capacity_kwh).percent_to_raw(percent)
    }

    fn raw_to_percent(bank: PowerLimitBank, raw: u16, max_w: u32, capacity_kwh: f32) -> u16 {
        PowerLimitScale::new(bank, max_w, capacity_kwh).raw_to_percent(raw)
    }

    fn raw_to_watts(bank: PowerLimitBank, raw: u16, max_w: u32, capacity_kwh: f32) -> f64 {
        PowerLimitScale::new(bank, max_w, capacity_kwh).raw_to_watts(raw)
    }

    // Expected values are taken from GivTCP, the only reference validated on
    // hardware, rather than from this module's own arithmetic.
    //
    // `write.py`: target = min(watts / (capacity_wh / 2) * 50, 50)
    // `read.py`:  watts  = min(reg / 100 * capacity_wh, inverter_max_w)
    fn giv_tcp_register_for(watts: f64, capacity_wh: f64) -> f64 {
        (watts / (capacity_wh / 2.0) * 50.0).min(50.0)
    }

    fn giv_tcp_watts_for(register: u16, capacity_wh: f64, inverter_max_w: f64) -> f64 {
        (f64::from(register) / 100.0 * capacity_wh).min(inverter_max_w)
    }

    const HALF: PowerLimitBank = PowerLimitBank::HalfScale;

    #[test]
    fn the_issue_346_reporter_gets_register_17_for_62_percent_not_31() {
        // Gen1 Hybrid, 9.5 kWh, 2600 W.
        assert_eq!(percent_to_raw(HALF, 62, 2600, 9.5), 17);
        // The register the old "percent / 2" wrote asks for 2945 W, above the
        // 2600 W inverter, so it limited nothing.
        assert_eq!(giv_tcp_watts_for(31, 9500.0, 2600.0), 2600.0);
        assert!((giv_tcp_watts_for(17, 9500.0, 2600.0) - 1615.0).abs() < 1e-9);
    }

    #[test]
    fn matches_giv_tcps_watts_to_register_formula_within_one_step() {
        for percent in 1..=99u16 {
            let watts = f64::from(percent) / 100.0 * 2600.0;
            let expected = giv_tcp_register_for(watts, 9500.0);
            let raw = percent_to_raw(HALF, percent, 2600, 9.5);
            assert!(
                (f64::from(raw) - expected).abs() <= 1.0,
                "{percent}%: wrote {raw}, GivTCP {expected}"
            );
            let delivered = giv_tcp_watts_for(raw, 9500.0, 2600.0);
            assert!(
                (delivered - watts).abs() <= 95.0,
                "{percent}%: delivers {delivered} W for {watts} W"
            );
        }
    }

    #[test]
    fn hundred_percent_writes_the_register_maximum_whatever_the_capacity() {
        // 100% means "no limit": register 50 (0.5C) is above any inverter's
        // rating, and unlike a derived 28 it cannot throttle if the capacity is
        // misread. It is also the factory default.
        for capacity_kwh in [0.0_f32, 2.6, 5.12, 9.5, 20.0] {
            assert_eq!(percent_to_raw(HALF, 100, 2600, capacity_kwh), 50);
        }
        assert_eq!(giv_tcp_watts_for(50, 9500.0, 2600.0), 2600.0);
    }

    #[test]
    fn zero_percent_writes_zero_and_the_register_never_exceeds_50() {
        assert_eq!(percent_to_raw(HALF, 0, 2600, 9.5), 0);
        for percent in 0..=300u16 {
            assert!(percent_to_raw(HALF, percent, 2600, 9.5) <= 50);
            assert!(percent_to_raw(HALF, percent, 2600, 2.0) <= 50);
        }
    }

    #[test]
    fn is_monotonic_across_the_slider() {
        let mut previous = 0;
        // Includes 100% -> 50, a deliberate jump past the derived 28.
        for percent in 0..=100u16 {
            let raw = percent_to_raw(HALF, percent, 2600, 9.5);
            assert!(raw >= previous, "{percent}%");
            previous = raw;
        }
    }

    #[test]
    fn a_small_pack_keeps_the_plain_half_scale() {
        // 5.12 kWh: the maximum is capacity / 2, so register 50 really is 100%.
        assert_eq!(percent_to_raw(HALF, 100, 2560, 5.12), 50);
        assert_eq!(percent_to_raw(HALF, 62, 2560, 5.12), 31);
        assert_eq!(percent_to_raw(HALF, 50, 2560, 5.12), 25);
        assert_eq!(raw_to_percent(HALF, 31, 2560, 5.12), 62);
        assert_eq!(raw_to_percent(HALF, 50, 2560, 5.12), 100);
    }

    #[test]
    fn unknown_capacity_or_maximum_falls_back_to_the_plain_half_scale() {
        for (max_w, capacity_kwh) in [(2600, 0.0), (0, 9.5), (0, 0.0), (2600, f32::NAN)] {
            assert_eq!(percent_to_raw(HALF, 62, max_w, capacity_kwh), 31);
            assert_eq!(percent_to_raw(HALF, 100, max_w, capacity_kwh), 50);
            assert_eq!(raw_to_percent(HALF, 31, max_w, capacity_kwh), 62);
            assert_eq!(raw_to_percent(HALF, 50, max_w, capacity_kwh), 100);
        }
    }

    #[test]
    fn a_maximum_stated_above_capacity_over_two_still_tops_out_at_register_50() {
        // The decoder caps the maximum at capacity / 2, but register 50 is 0.5C
        // however it is stated.
        assert_eq!(raw_to_percent(HALF, 50, 5000, 9.5), 100);
        assert_eq!(percent_to_raw(HALF, 100, 5000, 9.5), 50);
        for percent in 0..=100u16 {
            assert_eq!(
                percent_to_raw(HALF, percent, 5000, 9.5),
                percent_to_raw(HALF, percent, 4750, 9.5),
                "{percent}%"
            );
        }
    }

    #[test]
    fn reads_the_register_as_a_share_of_the_inverter_maximum() {
        assert_eq!(raw_to_percent(HALF, 17, 2600, 9.5), 62);
        for raw in 0..=50u16 {
            let watts = giv_tcp_watts_for(raw, 9500.0, 2600.0);
            let expected = (watts / 2600.0 * 100.0).round() as u16;
            assert_eq!(raw_to_percent(HALF, raw, 2600, 9.5), expected, "raw {raw}");
        }
    }

    #[test]
    fn registers_that_exceed_the_maximum_read_as_full_power() {
        // The factory default (50) and the old buggy 31 both reach the maximum.
        for raw in [28, 31, 50] {
            assert_eq!(raw_to_percent(HALF, raw, 2600, 9.5), 100, "raw {raw}");
        }
        assert_eq!(raw_to_percent(HALF, 255, 2600, 9.5), 100);
    }

    #[test]
    fn every_register_below_the_maximum_round_trips() {
        for raw in 0..=27u16 {
            let percent = raw_to_percent(HALF, raw, 2600, 9.5);
            assert_eq!(percent_to_raw(HALF, percent, 2600, 9.5), raw, "raw {raw}");
        }
    }

    #[test]
    fn direct_banks_are_a_plain_percentage_whatever_the_pack_size() {
        for bank in [PowerLimitBank::AcBank, PowerLimitBank::ThreePhase] {
            for capacity_kwh in [0.0_f32, 5.12, 9.5, 20.0] {
                assert_eq!(percent_to_raw(bank, 62, 3000, capacity_kwh), 62);
                assert_eq!(percent_to_raw(bank, 100, 3000, capacity_kwh), 100);
                assert_eq!(raw_to_percent(bank, 62, 3000, capacity_kwh), 62);
                assert_eq!(raw_to_percent(bank, 255, 3000, capacity_kwh), 100);
            }
        }
    }

    #[test]
    fn watts_follow_giv_tcps_read_formula_on_the_half_scale() {
        // read.py: min(reg / 100 * capacity_w, inverter_max). The reporter's
        // register 31 is 2945 W of request on 9.5 kWh, so the 2600 W inverter
        // is the real limit.
        for raw in 0..=60u16 {
            let expected = giv_tcp_watts_for(raw.min(50), 9500.0, 2600.0);
            let watts = raw_to_watts(HALF, raw, 2600, 9.5);
            assert!(
                (watts - expected).abs() < 1e-6,
                "raw {raw}: {watts} vs {expected}"
            );
        }
        assert!((raw_to_watts(HALF, 17, 2600, 9.5) - 1615.0).abs() < 1e-6);
        assert_eq!(raw_to_watts(HALF, 31, 2600, 9.5), 2600.0);
    }

    #[test]
    fn watts_fall_back_to_the_plain_half_scale_when_capacity_is_unknown() {
        // 33 doubled = 66% of 2600 W.
        assert!((raw_to_watts(HALF, 33, 2600, 0.0) - 1716.0).abs() < 1e-6);
        assert_eq!(raw_to_watts(HALF, 50, 2600, 0.0), 2600.0);
    }

    #[test]
    fn watts_on_a_direct_bank_are_a_percentage_of_the_maximum() {
        for bank in [PowerLimitBank::AcBank, PowerLimitBank::ThreePhase] {
            assert!((raw_to_watts(bank, 66, 3000, 9.5) - 1980.0).abs() < 1e-6);
            assert_eq!(raw_to_watts(bank, 255, 3000, 9.5), 3000.0);
        }
    }

    #[test]
    fn watts_are_zero_when_the_maximum_is_unknown() {
        assert_eq!(raw_to_watts(HALF, 25, 0, 9.5), 0.0);
        assert_eq!(raw_to_watts(PowerLimitBank::AcBank, 50, 0, 9.5), 0.0);
    }

    #[test]
    fn a_non_zero_percentage_never_writes_register_zero() {
        // 1% of 2600 W on a 9.5 kWh pack is register 0.27. Writing 0 would stop
        // charging (and the forecast reads 0 as "unset"), so the smallest real
        // limit is register 1; only a deliberate 0% writes 0.
        for (max_w, capacity_kwh) in [(2600, 9.5), (2600, 13.5), (3600, 20.0), (2560, 5.12)] {
            assert_eq!(percent_to_raw(HALF, 0, max_w, capacity_kwh), 0);
            for percent in 1..=99u16 {
                assert!(
                    percent_to_raw(HALF, percent, max_w, capacity_kwh) >= 1,
                    "{percent}% on {capacity_kwh} kWh / {max_w} W wrote register 0"
                );
            }
        }
        // And it reads back as the percentage that register really allows.
        assert_eq!(raw_to_percent(HALF, 1, 2600, 9.5), 4);
    }

    #[test]
    fn an_implausible_capacity_is_treated_as_unknown() {
        // HR 55 is a raw u16 with no sanitiser on single-phase inverters, so one
        // corrupt read can report thousands of kWh. Acting on it would collapse
        // the ratio and write register 0 or 1 for a 40% request.
        for capacity_kwh in [3355.0_f32, 151.0, 0.9, 0.0, f32::NAN, f32::INFINITY, -5.0] {
            let scale = PowerLimitScale::new(HALF, 2600, capacity_kwh);
            assert!(!scale.is_known(), "{capacity_kwh} kWh");
            assert_eq!(scale.percent_to_raw(40), 20, "{capacity_kwh} kWh");
            assert_eq!(scale.raw_to_percent(20), 40, "{capacity_kwh} kWh");
            assert!(
                (scale.raw_to_watts(20) - 1040.0).abs() < 1e-6,
                "{capacity_kwh} kWh"
            );
        }
        for capacity_kwh in [1.0_f32, 2.6, 9.5, 81.0, 150.0] {
            assert!(PowerLimitScale::new(HALF, 2600, capacity_kwh).is_known());
        }
        assert!(!PowerLimitScale::new(HALF, 0, 9.5).is_known());
    }

    /// `tests/fixtures/power-limit-vectors.json` is the cross-language contract:
    /// the TypeScript conversion in `src/lib/powerLimit.ts` is checked against
    /// the same vectors in `tests/lib/powerLimit.test.ts`, so the two cannot
    /// drift apart.
    #[test]
    fn matches_the_shared_cross_language_vectors() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/power-limit-vectors.json"
        ))
        .expect("power-limit-vectors.json must parse");
        let mut checked = 0;
        for entry in fixture["scales"].as_array().expect("scales array") {
            let bank = match entry["bank"].as_str().unwrap() {
                "half" => PowerLimitBank::HalfScale,
                "direct" => PowerLimitBank::AcBank,
                other => panic!("unknown bank {other}"),
            };
            let scale = PowerLimitScale::new(
                bank,
                entry["max_w"].as_u64().unwrap() as u32,
                entry["capacity_kwh"].as_f64().unwrap() as f32,
            );
            let name = entry["name"].as_str().unwrap();
            for write in entry["writes"].as_array().unwrap() {
                let percent = write["percent"].as_u64().unwrap() as u16;
                assert_eq!(
                    scale.percent_to_raw(percent),
                    write["raw"].as_u64().unwrap() as u16,
                    "{name}: write {percent}%"
                );
                checked += 1;
            }
            for read in entry["reads"].as_array().unwrap() {
                let raw = read["raw"].as_u64().unwrap() as u16;
                assert_eq!(
                    scale.raw_to_percent(raw),
                    read["percent"].as_u64().unwrap() as u16,
                    "{name}: read register {raw}"
                );
                checked += 1;
            }
        }
        assert!(checked > 150, "fixture shrank to {checked} vectors");
    }
}

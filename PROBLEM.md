# PV Energy Today (kWh) Reconstruction - Problem Summary

## Problem

The "PV Energy Today (kWh)" graph on the History page shows incorrect totals. Data from real inverter readings (~4000W) should show ~18kWh by mid-afternoon but shows only ~2-3kWh.

## Data Example (2026-06-19)

```
09:50:00  4026W   0.17 kWh
09:55:00  (slot)  0.33 kWh   <- stuck, not accumulating
10:00:00  (slot)  0.33 kWh
10:05:00  (slot)  0.33 kWh
...
10:50:00  (slot)  0.33 kWh
11:15:00  4403W   0.69 kWh   <- jumps up
11:45:00  (slot)  2.53 kWh    <- jumps up but still wrong
...
14:30:00  (slot)  2.53 kWh   <- stuck again
14:35:00  3389W   2.81 kWh
```

## Root Causes Identified

### 1. Slot-filler rows had solar_power=NULL

- Original slot-filler only inserted `today_solar_kwh`, not `solar_power`
- Reconstruction only processes rows where `solar_power IS NOT NULL`
- Slot rows were skipped but their stale `today_solar_kwh` values persisted

**Fix**: Slot rows now insert `solar_power=0` with interpolated `today_solar_kwh`

### 2. Gap threshold too small

- Gap threshold was 600 seconds (10 minutes)
- Real gaps in data: 85 min (09:50→11:15), 175 min (11:45→14:30)
- Gaps larger than threshold were skipped, no energy accumulated

**Fix**: Gap threshold increased to 14400 seconds (4 hours)

### 3. Power interpolation wasn't working

- Initially used only `prev_solar_power` (single reading)
- Then changed to interpolate: average of prev + current power
- This assumes a straight line through gaps for energy calculation

**Current logic**: For gaps up to 4 hours, interpolate power as average of surrounding readings

## Architecture

### Files Modified

- `src-tauri/src/history/mod.rs` - Solar reconstruction logic
- `src/lib/api.ts` - Frontend API (stopped sending UTC midnight for "today" range)
- `src/lib/historyRangeConfig.ts` - Domain calculation

### Flow

1. Poll loop reads inverter registers, inserts into DB
2. DB stores `timestamp` (seconds), `solar_power` (W), `today_solar_kwh` (kWh)
3. On DB open, `reconstruct_solar_kwh()` runs:
   - Step 3: Integrate `solar_power` into `today_solar_kwh` per day
   - Step 4: Write back computed values to existing rows
   - Step 5: Slot-filler inserts missing 5-minute slots with interpolated energy

### Key Code (Step 3 - Energy Integration)

```rust
if delta_secs > 0 && delta_secs < 14400 {  // gap <= 4 hours
    let power_kw = if prev_solar_power > 0 && solar_power > 0 {
        ((prev_solar_power + solar_power) / 2) as f64 / 1000.0  // interpolate
    } else if prev_solar_power > 0 {
        prev_solar_power as f64 / 1000.0
    } else if solar_power > 0 {
        solar_power as f64 / 1000.0
    } else {
        0.0
    };
    let delta_hours = delta_secs as f64 / 3600.0;
    accumulated_kwh += power_kw * delta_hours;
}
```

## What Was Tried

| Commit | Change | Result |
|--------|--------|--------|
| 89a34f6 | 5-min slot filler, local midnight API | Charts better but energy wrong |
| 952bac3 | Revert rolling average to prev_solar_power | Energy too low |
| 8af19c0 | Fix timestamp units (seconds) | Tests pass but energy still wrong |
| 95932d0 | Slot rows need solar_power=0 | Gap handling fixed |
| d5d8c62 | Don't accumulate when prev_solar_power=0 | Broke interpolation |
| 3163ac1 | Interpolate power across gaps | Test updated, energy better |
| ef51c44 | Gap threshold 4 hours | Current |

## Test Results

All 459 Rust tests pass.

## Expected Behavior

- Real solar_power readings should accumulate energy correctly
- Gaps between readings should interpolate power (straight line)
- Slot-filler rows should fill gaps in the chart but not affect energy calculation
- Energy should reach ~18kWh by mid-afternoon with ~4000W input

## Current Status

- Still debugging - the interpolation might not be working correctly
- Slot rows may be overwriting correct values with interpolated ones
- Need to verify the calculation is correct in all cases

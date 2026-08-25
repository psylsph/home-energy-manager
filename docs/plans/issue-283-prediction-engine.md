# Design: Forecast & Planning (issue #283)

Status: **Phases 0–2 implemented; Phases 3–4 planned**
Issue: <https://github.com/psylsph/home-energy-manager/issues/283>

## Summary

Add a prediction engine that answers the question the issue poses: *given my
house, battery, solar generation, weather and tariffs, what should the system
do today?* The deliverable is a dedicated **Forecast** page showing predicted
solar generation, expected consumption, projected battery SOC and expected
import/export — with confidence bands — plus (in a later phase) tariff-aware
recommendations the user can apply with one tap.

The approach is **deterministic, local and explainable**: no LLM in the loop.
The output reads like advice ("expect 18.4 kWh tomorrow; charge 4 kWh
overnight") but is computed from Open-Meteo radiation forecasts and the user's
own history in `history.db`. A local model runs offline, is free, private,
auditable and unit-testable — none of which apply to a hosted AI service.

## Decisions (settled)

| # | Question | Decision |
|---|---|---|
| 1 | Where does it live in the UI? | Dedicated **Forecast page** (route `/forecast`), not a StatusPage panel or HistoryPage overlay |
| 2 | Solar forecast provider | **Open-Meteo only** for v1 (keyless, already integrated). Provider sits behind an internal trait so alternatives can slot in later |
| 3 | Automation mode | **Deferred** — decide when Phases 1–2 have shipped and accuracy data exists |
| 4 | Efficiency defaults | Charge (AC→battery) **90%**, discharge (battery→AC) **95%**; capacity from the dongle's `battery_capacity_kwh` with an optional user override. Both user-adjustable; self-calibration possible later |

### Provider options considered

- **Solcast** — does have a free hobbyist tier ("My home PV system only",
  one location, 2 tilt/azimuth combos) but it is capped at 10 API calls/day
  for new accounts and requires signup + API key. Not v1; possible Phase 4
  optional provider for users who already have accounts.
- **forecast.solar** — free and keyless, but low resolution, single plane,
  infrequent updates, and requires tilt/azimuth/kWp as inputs. That last
  point is the dealbreaker: the self-calibrating PR model (below) exists
  precisely so we never have to ask users for panel geometry.

## What already exists (building blocks)

The codebase already provides most of the plumbing this feature needs:

- **Time-series data** — `history.db` `readings` table: `today_solar_kwh`,
  `today_consumption_kwh`, SOC, battery power, charge/discharge rates. Enough
  to fit empirical consumption profiles and calibrate a solar model.
- **Weather integration** — `weather/mod.rs` polls Open-Meteo every 15 min
  (temperature only today) with a dedicated ureq agent, `WeatherState`,
  settings UI and archive backfill. The forecast extension grows from here.
- **Tariff awareness** — multi-slot tariff config (`tariff.ts`, covers Flux),
  `octopus_tariff_prices` table (Agile forward prices), import/export tariff
  settings, standing charge.
- **Automation precedent** — Agile (`evaluate_agile_slot` in
  `state_machines.rs`), Auto Winter, and Adaptive Charge provide the opt-in /
  safety / reporting patterns any future automation phase must follow.
- **Actuators** — the full write pipeline (charge/discharge slots incl.
  10-slot extended, reserve SOC, rates, enable flags).
- **System metadata** — `battery_capacity_kwh` from the dongle;
  `solar_arrays` config with per-array `rated_kw`.

## Solar model

Fetch hourly `shortwave_radiation`, `direct_radiation` and `cloud_cover` for
a +48 h horizon from Open-Meteo's free forecast endpoint (same API already
used for temperature, CC-BY 4.0 attribution already displayed).

Convert radiation to PV power with:

```text
P_hour = (G_hour / 1000) × Σ rated_kw(solar_arrays) × PR
```

`PR` (performance ratio) is **self-calibrated** from the trailing 7–14 days
of actual `today_solar_kwh` versus the delivered radiation integral over the
same period. This absorbs tilt, azimuth, orientation, shading and soiling
without asking the user for panel geometry. The model refuses to present
confidence until roughly 14 days of history exist.

Confidence bands come from historical forecast-error quantiles (tracked once
`forecast_values` rows accumulate), not from a hand-picked constant.

## Battery simulation & efficiency

Deterministic hourly step simulation over the forecast horizon:

1. Predicted solar feeds home load first.
2. Surplus charges the battery at the configured charge rate, scaled by
   **charge efficiency (default 90%)**, until full; remaining surplus exports.
3. Deficit discharges the battery at the discharge rate, scaled by
   **discharge efficiency (default 95%)**, down to the reserve SOC; remaining
   deficit imports.

Round-trip ≈ 85.5%, in line with common assumptions for GivEnergy LV systems.
Capacity defaults to the dongle's `battery_capacity_kwh` (GivEnergy SOC maps
to the usable window, so no DoD derating) with a user override for packs that
read undersized. Phase 4 may self-calibrate efficiencies from
`today_charge_kwh` versus actual import during charge windows.

## Phases

### Phase 0 — forecast data foundation (backend, no UI) — **shipped**

- `src-tauri/src/forecast/` — `solar.rs` (Open-Meteo provider behind
  `SolarForecastProvider`, URL builder + pure response parser),
  `calibration.rs` (PR fit), `mod.rs` (`store_and_calibrate` seam,
  `effective_solar_kwp`, `run_solar_forecast_fetch` tick entry point).
- The fetch rides the weather loop on a 3 h tick (`past_days=14`
  modelled radiation for calibration, `forecast_days=3` forward).
- `forecast_values` table in `history.db`
  (`timestamp, variable, value, source, fetched_at`, upsert per
  (timestamp, variable, source)) — the basis for predicted-vs-actual
  accuracy tracking and honest confidence bands.
- `daily_solar_totals_since` query + PR self-calibration: median daily
  ratio of actual `today_solar_kwh` vs delivered insolation × rated
  kWp, rejecting dark / partial-coverage / dead days; needs ≥ 5 usable
  days before reporting a value.
- Tests: JSON parser tables (mirroring `parse_archive_response`),
  calibration maths, upsert/query round-trip, kWp selection rules.

### Phase 1 — forecast module, API and page — **shipped**

- `forecast/consumption.rs` — hour-of-day profile (median + p25/p75)
  fitted from `home_energy_today_kwh` counters (`today_consumption_kwh`
  fallback, same precedence as `query_energy_summary`); deltas guarded
  against >2 h gaps, midnight resets and negative glitches; 7-day
  sufficiency gate; nearest-neighbour fallback for unobserved hours.
- `forecast/simulate.rs` — hourly SOC projection: efficiency-scaled
  charge/discharge, rate caps (model-aware kW from
  `DeviceType::uses_direct_charge_limit` + `max_battery_power_w`),
  reserve floor, room clamp.
- Settings: `forecast_charge_efficiency` (0.9) and
  `forecast_discharge_efficiency` (0.95), range-validated both sides,
  editable on the Settings page's Solar section.
- Calibrated PR persisted to `history.meta` (`forecast_pr`,
  `forecast_pr_days`, `forecast_calibrated_at`) at fetch time; payload
  falls back to a 0.75 PR flagged `calibrating`.
- `GET /api/forecast` — assembled on request from local state only
  (no network on the request path); degradation codes
  (`weather_disabled`, `no_coordinates`, `no_forecast_data`,
  `calibrating`, `insufficient_consumption_history`, `no_snapshot`,
  `no_battery_capacity`) with partial data always served.
- `ForecastPage.tsx` at `/forecast` (nav after History): Tomorrow
  summary card, 48 h solar chart (±20 % model band until Phase 4
  replaces it with measured error quantiles), consumption profile
  chart with band, battery projection with reserve line, status banner,
  Open-Meteo attribution.

Page layout, top to bottom:

1. **Tomorrow summary card** — solar kWh + band, consumption kWh + band,
   current SOC, expected surplus/import.
2. **Solar chart** — predicted hourly kW with confidence band and a
   seasonal-average dashed line; today's actuals overlaid as the day runs.
3. **Consumption chart** — median profile with p25–p75 band.
4. **Battery projection chart** — simulated SOC curve to midnight with
   reserve/target SOC reference lines.
5. *(Phase 2)* **Plan card** — recommendation with rationale and Apply.

### Phase 2 — planner (issue #283) — **shipped**

- `forecast/planner.rs` — consumes the Phase 1 SOC trajectory plus the
  user's import-tariff config and returns a `PlanRecommendation`:
  - `NoChargeNeeded { projected_end_soc_pct }` when the projection
    already ends above the target;
  - `Charge { window, kwh, target_soc_pct, projected_end_soc_pct,
    rationale }` when the cheapest forward import window can close the
    gap, AC kWh asked for clamped by capacity / rate / window time;
  - `NoPlan { reason }` for missing-tariff / insufficient-history /
    no-projection.
- `cheapest_import_window(tariff, now_min, min_duration_min)` rotates
  slot starts into the forward 24 h so a 02:00–05:00 off-peak is
  always reachable; cheaper-first with past-skipped and duration-floor
  filtering.
- `GET /api/forecast/plan` assembles the recommendation from the Phase
  1 payload plus settings and snapshot, and serialises an `apply`
  block that mirrors the Control page's POST bodies
  (`/api/control/charge-slot`, `/api/control/timed-charge`).
- `ForecastPage.tsx` Plan card: `forecastPlanTitle` headline per kind,
  Apply button wired to those two endpoints, no button on
  no-charge / no-plan states. No autonomy — the user always clicks.

### Phase 2 — originally proposed (now superseded above)

- `forecast/planner.rs`: tariff-aware planner. For Flux-style tariffs the
  output collapses to "charge X kWh in the cheapest fixed window to hit
  target SOC by the export peak"; when Agile prices are present it evaluates
  price slots instead.
- Plan card with one-tap **Apply** → existing `/api/control/charge-slot` and
  timed-charge endpoints. Nothing autonomous; the user confirms.
- Optional: plan digest appended to the existing daily report
  (Telegram/ntfy).
- Tests: cheapest-window selection, no-charge-needed branch, reserve-floor
  clamp, insufficient-history fallback.

### Phase 3 — automation mode (deferred)

If built, it must mirror the Agile pattern: explicit opt-in, always-visible
"automation active — last plan: …" banner plus plan history, skip writes when
the user has made manual changes recently, and hard safety rails (respect
reserve SOC, max charge rate, cold-battery charge limits, never arm
discharge with no slot configured). **Decision deferred until Phases 1–2
have shipped.**

### Phase 4 — trust building & extras

- Predicted-vs-actual accuracy chart from `forecast_values`.
- Efficiency self-calibration from history.
- Optional second provider (Solcast hobbyist) behind the existing trait.
- The `GET /api/forecast` endpoint doubles as the "add-on platform" the
  issue author wanted — external tooling (e.g. a Raspberry-Pi predictor)
  can consume it directly.

## Testing strategy

Per repo policy, every behaviour ships with tests:

- **Rust** — inline `#[cfg(test)]` modules: response parsing tables,
  PR/efficiency maths, simulator step logic, planner branches, degradation
  states. No network in tests.
- **`tests/e2e_mock.rs`** — `GET /api/forecast` happy path and degradation
  responses from day one.
- **Frontend** — pure helpers in `tests/lib/forecast*.test.ts` (band maths,
  formatting, sufficiency states) and component tests in
  `tests/pages/forecastPage*.test.tsx` mirroring the existing page-test
  patterns.

## Risks / watch-items

- **Don't trust forecasts blindly** — every predicted number needs a
  data-sufficiency gate; never render guesswork as confident output.
- **Accuracy trust** — ship display (Phase 1) and recommendations (Phase 2)
  before any automation so users can watch accuracy first.
- **Attribution** — extend the existing Open-Meteo attribution wording to
  cover forecast data (CC-BY 4.0 requirement).
- **Single-efficiency limitation** — one charge/discharge efficiency pair
  won't fit every battery; user-adjustable from day one, self-calibrating
  later.

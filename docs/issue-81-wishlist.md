# Issue #81 — UI Wish List (detailed todo)

Source: <https://github.com/psylsph/home-energy-manager/issues/81>
Reporter: @gagenp01 · Label: `enhancement` · Priority: non-urgent (reporter's words: "none urgent").

## Reporter's overall goal

> "My objective … is to have all the relevant info in the tab as you select along the bottom. The way I like to use your program is to step (select with mouse) from left to right as I gather the status of my system."

So each bottom-nav tab should be **self-contained** — a user should be able to read a full status picture without jumping between tabs.

---

## Workstream A — Make the Battery tab overview hideable via a Settings toggle

> **Reframed 2026-06-16** based on the comment thread on issue #81. Originally scoped as "add a SOC footer at the bottom of the Battery page"; the conversation clarified the reporter's actual intent (see below).

### Original request (for context)

> "Is it possible to have the battery SOC included at the bottom of the battery page or a switch to enable it? It does not matter if it scrolls off the bottom if you open the battery details module?"

### Update (2026-06-16 — issue comments)

A 3-comment thread today clarified the real intent:

1. **@psylsph**: pushed back on adding a second SOC display — "The battery SOC is already shown at the top of the page why do you want at the bottom? I was actually thinking of removing the overview from the Battery tab and just leaving the module data as it's just a repeat of what's on the Status page anyway."
2. **@gagenp01**: confirmed that was exactly their idea — quoted the maintainer's "remove the overview" line and attached a screenshot labelled "This is my idea. If you have something else then fine on that too."
3. **@psylsph**: "got you, will have a look — will probably make it a toggle option under settings." (+1)

**Net effect:** the reporter doesn't want a *second* SOC card at the bottom. They agree the Battery tab overview duplicates the Status page and want the **option to hide it**, leaving just the per-module battery detail. The agreed plan is a **Settings toggle**.

### Current behaviour

`src/pages/BatteryPage.tsx` renders the `BatteryPanel` (SOC ring + power/voltage/temp/mode/reserve/charged-discharged) **once, at the top**. This overview duplicates most of the Status page's battery summary.

`src/components/BatteryPanel.tsx` is a `memo`-ised component that already takes a `snapshot` and renders the full SOC card.

### Revised approach

Two complementary pieces. **A2 is the active direction** (maintainer, 2026-06-16); A1 is an optional companion.

#### A1 (optional / companion) — Settings toggle to hide the overview card

Add a **Settings toggle that hides the Battery tab overview** (`BatteryPanel` at the top), so the page shows only the per-module cell detail. Default **on** (overview shown) to preserve current behaviour; users who find it redundant can turn it off.

#### A2 (primary, current direction) — "Today's SOC" chart at the bottom of the Battery page

Replicate the History → Battery → "SOC %" chart on the Battery tab, pinned to **today**. This gives the Battery tab something the Status page *doesn't* have (a SOC-over-time trend), serving the reporter's "self-contained tab" goal — and if A1 later hides the overview card, this chart becomes the on-tab SOC reference (resolving the original request cleanly).

"SOC %" chart definition to replicate (from `HistoryPage.tsx`):
`{ field: 'soc', color: '#6366F1', yDomain: [0,100], unit: '%' }`, fetched via `fetchHistory('today', ['soc'], 0, false)`.

### Tasks

**A1 (optional):**

- [ ] Add a Zustand-persisted boolean `showBatteryOverview` (default **on**) to `src/store/useInverterStore.ts`, mirroring the `developerMode` / `chartRange` pattern.
- [ ] In `src/pages/BatteryPage.tsx`, gate the top `BatteryPanel` render on the new flag.
- [ ] Add the toggle under a "Battery" or "Display" section in `src/pages/SettingsPage.tsx`, reusing the existing `Toggle` component.

**A2 (primary):**

- [ ] New component `src/components/BatterySocChart.tsx` (no props): fetches `fetchHistory('today', ['soc'], 0, false)`, SOC spike-filters the series, renders a Recharts `AreaChart` (stroke `#6366F1`, gradient fill, `yDomain={[0,100]}`, height ~180px). States: loading / empty / error (mirror History/Power pages).
- [ ] Refresh: `useNow()` (60s) + `refreshKey = shouldRefreshHistoryRange('today', 0) ? now : 0` in the fetch effect deps — identical to History/Power pages.
- [ ] Reuse from `historyRangeConfig.ts`: `getHistoryRangeDomain('today',0,now)`, `getHistoryXAxisTicks`, `formatHistoryXAxisTick`, `getHistoryXAxisMinTickGap`, `HISTORY_CHART_GRID_PROPS`, `shouldRefreshHistoryRange`. No new patterns.
- [ ] SOC spike filter: **recommend** extracting `removeSpikes` + `SPIKE_THRESHOLDS` (currently module-local in `HistoryPage.tsx`) into `src/lib/chartSeries.ts` and reusing from both; minimal alternative is an inline SOC-only filter (threshold 15).
- [ ] Insert `<BatterySocChart />` into `BatteryPage.tsx` at the bottom of the page (after the modules / no-modules section). Decision pending: bottom-of-page (recommended) vs directly under the `BatteryPanel` overview card.

**Both:**

- [ ] Manually verify (A2): chart fills in over the day, refreshes each minute, shows the empty state on a fresh install; disconnected/no-battery states render sensibly.
- [ ] `npm run lint` + `npm run lint:md` + `npm run build` clean.

### Notes / decisions to make

- The original "SOC footer chip" idea is **dropped** — replaced by the richer A2 SOC chart.
- A2 placement: **bottom-of-page** (recommended, additive, matches the Solar-wishlist bottom-graph pattern) vs **under the overview card** (groups SOC-now + SOC-today).
- A1 default: keep overview **on** (safe) vs **off** (matches the maintainer's lean toward removing it). A2 makes the overview less redundant, so **on** is the safer default.
- No backend/Modbus changes for either — `soc` is already recorded in history; the toggle is pure frontend state.

---

## Workstream B — Solar page: hide PV2 + add a PV output graph

### What was asked

> "…a switch to turn off PV2 to those of us that only have one PV installed? And, if possible, to have the solar PV output graph included? This could be at the bottom of the page or the graph at the side [of the] input breakdown — I mean have a thin bar for the PV output of each string at the LHS with the graph filling in the rest of the space."
>
> "These graphs would be extra (repeat) of what you have in other tabs **not moving** the content from the other graphs (power and history)."

So: **additive** only. Do not remove the existing Power/History PV graphs.

### Current behaviour

`src/pages/SolarPage.tsx`:

- "Solar Overview" → total solar power (big amber number).
- "Input Breakdown" → PV1 + (PV2) bars. PV2 only renders when `hasPv2 = snapshot.pv2_voltage > 0 || snapshot.pv2_power > 0` (auto-detect).
- Detail cards for PV1 and PV2.
- **No time-series graph** of PV output on this page. PV power-over-time currently only lives on Power page and the History → Solar tab.

The History API already serves `pv1_power` / `pv2_power` / `today_solar_kwh` (see `getCharts('solar', …)` in `HistoryPage.tsx`), so data is available via `fetchHistory()` from `src/lib/api.ts`.

### Part B1 — Manual "hide PV2" switch

- [ ] Add a Zustand-persisted boolean `hidePv2` (default **off**) to `useInverterStore.ts` (`localStorage` key e.g. `hidePv2`).
- [ ] In `SolarPage.tsx`, change the PV2 visibility logic from pure auto-detect to `showPv2 = !hidePv2 && (pv2_voltage > 0 || pv2_power > 0)`. When `hidePv2` is on, never render PV2 (bar, card, or overview mention).
- [ ] Add the toggle to Settings (a "Solar" sub-section) using the existing `Toggle` component.
- [ ] Edge case: confirm "Input Breakdown" still lays out sensibly with a single bar (it already does — PV2 is conditionally rendered).
- [ ] Verify with a single-string user that PV2 flicker-on from transient readings is gone.

### Part B2 — PV output time-series graph (additive)

Two placement options were offered; **pick one** (reporter seems to mildly prefer the side-by-side layout, but "bottom of the page" is explicitly acceptable).

**Recommended: bottom-of-page stacked area chart** (simplest, matches History styling, lowest risk).

- [ ] Create a new component `SolarPowerChart.tsx` (or inline in `SolarPage`) that reuses the History time-range machinery: read shared `chartRange` from the store and call `fetchHistory(range, ['pv1_power','pv2_power','today_solar_kwh'], …)`.
- [ ] Render a stacked PV1/PV2 power area chart using Recharts `AreaChart` (mirror the `ChartCard` styling in `HistoryPage.tsx`: gradient fill, `HISTORY_CHART_GRID_PROPS`, font sizes).
- [ ] Respect the B1 `hidePv2` switch (omit the PV2 series when hidden).
- [ ] Add a second small "PV Energy Today" area or stat under it (reuses `today_solar_kwh`).
- [ ] Insert below the existing detail cards in `SolarPage.tsx`.
- [ ] Loading / empty / disconnected states (reuse the existing spinner/empty patterns already in `SolarPage`/`HistoryPage`).

**Optional / stretch: side-by-side layout** ("thin bar for each string at LHS, graph filling rest").

- [ ] If pursuing: replace the "Input Breakdown" section with a flex row — narrow vertical bars (PV1 / PV2 current output) on the left, the time-series chart on the right. Treat as a separate sub-task / follow-up; the bottom-of-page version satisfies the request on its own.

### Tasks common to B

- [ ] Confirm the new chart shares the `chartRange` selector so the time window is consistent with Power/History tabs.
- [ ] `npm run lint` + `npm run build` clean.
- [ ] Manual check on a 2-string system and (via the mock/simulator) a 1-string system.

### Notes / decisions to make

- Reuse `fetchHistory` + existing range config — do **not** build a parallel history fetch path.
- Keep series colours consistent with History's Solar tab (`PV1 #F59E0B`, `PV2 #3B82F6` — already defined in `SolarPage`'s `pvColor()`).
- "Repeat, not move" — explicitly leave the Power-page and History Solar-tab graphs untouched.

---

## Workstream C — Lockable vertical (Y-axis) scale for graphs

### What was asked

> "Is it possible to have a setting in the settings tab to control/lock the vertical scale? Your graphs are stable and better than any other solution I have seen … fantastic compared to GivEnergy's ever-changing scale of their portal horror."

### Current behaviour

- **Power page** (`src/pages/PowerPage.tsx`): the power Y-axis comes from `calculateDomain(rows)` → rounds the data max up to the next 1000 W, symmetric ±. It **rescales** as data changes. The SOC axis is fixed `[0, 100]`.
- **History page** (`src/pages/HistoryPage.tsx`): each `ChartDef` has an optional `yDomain`. Only SOC uses a fixed `[0, 100]`; power/energy/voltage/cost charts pass `undefined` → Recharts auto-scales to the visible window.
- No user-facing way to pin a fixed W maximum.

### Proposed approach

Add a "Lock graph vertical scale" setting: when on, power charts use a **user-defined fixed W maximum** instead of auto-scaling. Keep SOC at `[0,100]`. Offer a small set of presets (e.g. 3 / 5 / 7.5 / 10 / 15 kW) plus "Auto".

### Tasks

- [ ] Add Zustand-persisted state for the lock: e.g. `chartPowerScaleMax: number | null` (`null` = Auto). Persist in `localStorage` like `chartRange`.
- [ ] In `PowerPage.tsx`, thread the value into `calculateDomain` (or compute the domain directly): if a max is set, return `[-max*1000, max*1000]`; else current auto behaviour.
- [ ] In `HistoryPage.tsx`, when the lock is on and a chart's unit is `W`, set its `yDomain` to `[-max*1000, max*1000]` (or `[0, max*1000]` for non-negative series) instead of leaving it undefined. Leave `%`, `V`, `kWh`, `£` charts on their existing domains.
- [ ] Add a Settings section "Graphs" (or extend an existing display section) with:
  - [ ] A toggle to enable the lock.
  - [ ] A kW preset selector (3 / 5 / 7.5 / 10 / 15 kW buttons, matching the Refresh Interval button-row style).
- [ ] Make sure the lock applies **consistently** across Power + History so the reporter's "stable scale" goal is met when stepping left→right.
- [ ] Verify SOC axis stays fixed `[0,100]` regardless of the lock.
- [ ] `npm run lint` + `npm run build` clean.

### Notes / decisions to make

- Decide unit: the reporter talks about "vertical scale" generically; power (W) is the pain point ("ever-changing scale"). Scope the lock to **power charts** first; voltage/energy can stay auto (and kWh/£ are cumulative, where auto is fine). Confirm this scoping is acceptable, or extend later.
- Negative side: Power page is symmetric (`±max`); History power charts can go negative too (import/export, charge/discharge are split into positive series). Decide per-chart whether to use `[0, max]` or `[-max, max]`.
- This is a frontend-only change — no backend/API work required.

---

## Cross-cutting / rollout

- [ ] All three workstreams are **frontend-only** (Zustand + Settings + page components). No Rust/Modbus changes.
- [ ] Persisted settings all go through the `localStorage` pattern already established in `useInverterStore.ts`.
- [ ] Settings UI additions should reuse the existing `Toggle` and button-row components in `SettingsPage.tsx` for visual consistency.
- [ ] After implementation: `npm run lint`, `npm run lint:md` (this file + any touched docs), `npm run build`.
- [ ] Optional: post a summary comment on issue #81 noting which items are implemented (do **not** close the issue unless asked).

# Issue #81 — UI Wish List (detailed todo)

Source: <https://github.com/psylsph/home-energy-manager/issues/81>
Reporter: @gagenp01 · Label: `enhancement` · Priority: non-urgent (reporter's words: "none urgent").

## Reporter's overall goal

> "My objective … is to have all the relevant info in the tab as you select along the bottom. The way I like to use your program is to step (select with mouse) from left to right as I gather the status of my system."

So each bottom-nav tab should be **self-contained** — a user should be able to read a full status picture without jumping between tabs.

---

## Workstream A — Battery SOC at the bottom of the Battery page

### What was asked

> "Is it possible to have the battery SOC included at the bottom of the battery page or a switch to enable it? It does not matter if it scrolls off the bottom if you open the battery details module?"

### Current behaviour

`src/pages/BatteryPage.tsx` renders the `BatteryPanel` (SOC ring + power/voltage/temp/mode/reserve/charged-discharged) **once, at the top**. When a module is expanded and the page scrolls, that SOC card scrolls out of view. There is no bottom reference.

`src/components/BatteryPanel.tsx` is a `memo`-ised component that already takes a `snapshot` and renders the full SOC card.

### Proposed approach

Add a **compact, always-visible SOC footer** on the Battery page (and a Settings toggle to hide it for people who don't want it). Compact = SOC % + state badge + maybe power, not the whole card — to avoid a wall of duplicate text.

### Tasks

- [ ] Decide footer content: SOC % + state (charging/discharging/idle) + power. Keep it one line.
- [ ] Add a Zustand-persisted boolean `showBatterySocFooter` (default **on**), mirroring the pattern used for `developerMode` / `chartRange` in `src/store/useInverterStore.ts`.
- [ ] In `BatteryPage.tsx`, render the compact footer at the end of the page (after the modules section / the "no battery modules" section) gated on the new flag.
- [ ] (Optional polish) Make the footer `sticky bottom` within the scroll container so it stays on screen while scrolling module details — confirm it doesn't fight the global bottom nav (`pb-safe`).
- [ ] Add a Settings toggle under a new "Battery" or "Display" section in `src/pages/SettingsPage.tsx` (reuse the existing `Toggle` component).
- [ ] Manually verify: expand a module, scroll, confirm SOC stays readable; toggle off in Settings, confirm footer disappears.
- [ ] `npm run lint` + `npm run build` clean.

### Notes / decisions to make

- Reporter explicitly accepts the footer scrolling off "if you open the battery details module" — so a plain bottom card is acceptable; sticky is a bonus, not required.
- Don't duplicate the *entire* `BatteryPanel` at the bottom (too noisy) — just SOC + state.

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

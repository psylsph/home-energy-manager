# Plan: Multi-Zone Tariff Cost Calculations (Flux & Beyond)

**Issue**: [#64 — Inclusion of Octopus Flux tariff for cost accounting](https://github.com/psylsph/home-energy-manager/issues/64)

**Goal**: Add time-of-use tariff support so the cost graphs on the History page correctly account for tariffs with 3+ rate zones (Octopus Flux, Cosy, etc.), without breaking existing 2-zone cost calculations.

---

## Current State

### Data model

**Backend** (`settings/mod.rs`): `TariffConfig` — 2-zone only:

```rust
pub struct TariffConfig {
    pub peak_rate: f64,        // £/kWh
    pub off_peak_rate: f64,    // £/kWh
    pub off_peak_start: String, // "HH:MM"
    pub off_peak_end: String,   // "HH:MM" (can cross midnight)
}
```

Separate `import_tariff_config` and `export_tariff_config` fields on `Settings`.

**Frontend** (`src/lib/types.ts`): mirrors this exactly.

### Cost calculation (`HistoryPage.tsx`)

The `preprocess` function for cost charts computes running cost via:

1. Delta of cumulative `today_import_kwh` / `today_export_kwh` between consecutive data points
2. Binary rate lookup: `isOffPeak(timestamp, start, end) ? off_peak_rate : peak_rate`
3. Accumulate: `acc += delta × rate`

### The gap

The 2-zone model cannot represent Octopus Flux (or Cosy), which have 3+ rate zones with different import and export rates per zone.

---

## Tariff Structures to Support

| Tariff | Zones | Times (approx) | Import rates | Export rates |
|---|---|---|---|---|
| **Standard** (current) | 2 | Off-peak window (configurable) | peak, off-peak | peak, off-peak |
| **Octopus Flux** | 3 | Off-peak 02:00–05:00, Peak 16:00–19:00, Day = rest | off-peak ~8p, day ~26p, peak ~42p | off-peak ~10p, day ~15p, peak ~22p |
| **Cosy Octopus** | 4 | Cosy: 04:00–07:00 + 13:00–16:00 + 22:00–00:00, Peak 16:00–19:00, Day = rest | cosy ~13p, day ~27p, peak ~40p | N/A (import only) |
| **Octopus Go** | 2 | Off-peak 00:30–05:30 | off-peak ~7p, peak ~28p | flat rate |

**Key insight**: Flux is 3 zones. Cosy is 4 zones (3 cheap periods + 1 peak + day). A generic N-zone model covers all cases and future-proofs for new tariffs.

---

## Proposed Design: Multi-Zone Tariff

### Phase 1 — Data Model (Backend + Frontend)

#### New Rust types (`settings/mod.rs`)

```rust
/// A single time-of-use rate zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TariffZone {
    /// Human-readable label (e.g. "Off-peak", "Day", "Peak", "Cosy").
    pub label: String,
    /// Rate in £/kWh for this zone.
    pub rate: f64,
    /// Zone start time "HH:MM" (24h).
    pub start: String,
    /// Zone end time "HH:MM" (24h). Can cross midnight.
    pub end: String,
}

/// Multi-zone tariff configuration (replaces TariffConfig for new mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiZoneTariffConfig {
    /// Ordered list of rate zones. Unmatched time falls to the "day" (default) rate.
    /// Zones must not overlap.
    pub zones: Vec<TariffZone>,
    /// Default rate for any time not covered by a zone (£/kWh).
    pub default_rate: f64,
}
```

#### New fields on `Settings`

```rust
pub struct Settings {
    // ... existing fields ...

    /// Tariff mode: "standard" (2-zone legacy) or "multizone" (N-zone).
    /// Default: "standard" — existing users unaffected.
    #[serde(default = "default_tariff_mode")]
    pub tariff_mode: String,

    /// Multi-zone import tariff config. Used when tariff_mode == "multizone".
    #[serde(default)]
    pub multizone_import_config: Option<MultiZoneTariffConfig>,

    /// Multi-zone export tariff config. Used when tariff_mode == "multizone".
    #[serde(default)]
    pub multizone_export_config: Option<MultiZoneTariffConfig>,
}
```

**Backward compatibility**: `import_tariff_config` / `export_tariff_config` are untouched. `tariff_mode` defaults to `"standard"`. All existing settings files load unchanged.

#### Preset configs (Rust constants or functions)

Provide factory functions for common tariffs so the UI can pre-fill:

```rust
impl MultiZoneTariffConfig {
    pub fn flux_import() -> Self {
        Self {
            zones: vec![
                TariffZone { label: "Off-peak".into(), rate: 0.078, start: "02:00".into(), end: "05:00".into() },
                TariffZone { label: "Peak".into(), rate: 0.420, start: "16:00".into(), end: "19:00".into() },
            ],
            default_rate: 0.265, // Day rate
        }
    }

    pub fn flux_export() -> Self {
        Self {
            zones: vec![
                TariffZone { label: "Off-peak".into(), rate: 0.098, start: "02:00".into(), end: "05:00".into() },
                TariffZone { label: "Peak".into(), rate: 0.220, start: "16:00".into(), end: "19:00".into() },
            ],
            default_rate: 0.150, // Day rate
        }
    }

    pub fn cosy_import() -> Self { /* 3 cosy + 1 peak zones */ }
    pub fn octopus_go_import() -> Self { /* single off-peak zone */ }
}
```

**Note**: Zones only list the *non-default* windows. The `default_rate` covers everything else. This means Flux has 2 zones (off-peak + peak) + a day default; Cosy has 4 zones (3 cosy + peak) + a day default.

#### TypeScript types (`src/lib/types.ts`)

```typescript
export interface TariffZone {
  label: string;
  rate: number;       // £/kWh
  start: string;      // "HH:MM"
  end: string;        // "HH:MM"
}

export interface MultiZoneTariffConfig {
  zones: TariffZone[];
  default_rate: number;  // £/kWh — the "day" rate
}

export type TariffMode = 'standard' | 'multizone';
```

Add to `PollSettings`:

```typescript
export interface PollSettings {
  // ... existing ...
  tariff_mode: TariffMode;
  multizone_import_config: MultiZoneTariffConfig | null;
  multizone_export_config: MultiZoneTariffConfig | null;
}
```

### Phase 2 — Settings API

#### `GET /api/settings` response (additive)

```json
{
  "import_tariff": 0.285,
  "export_tariff": 0.15,
  "import_tariff_config": { "peak_rate": 0.285, "off_peak_rate": 0.09, "off_peak_start": "00:30", "off_peak_end": "05:30" },
  "export_tariff_config": { "peak_rate": 0.15, "off_peak_rate": 0.15, "off_peak_start": "00:30", "off_peak_end": "05:30" },
  "tariff_mode": "standard",
  "multizone_import_config": null,
  "multizone_export_config": null
}
```

#### `PUT /api/settings` (accepts new fields)

Same shape. Backend validates:
- `tariff_mode` is `"standard"` or `"multizone"`
- When `"multizone"`, the `multizone_*_config` must be present
- Zone times are valid `"HH:MM"` format
- Zone times don't overlap (optional validation, warn only)

**Migration path**: When a user switches from `"standard"` to `"multizone"`, convert the existing 2-zone config into a `MultiZoneTariffConfig` with one zone (the off-peak window) and `default_rate` = the old `peak_rate`. This gives a smooth transition.

### Phase 3 — Settings UI (`SettingsPage.tsx`)

#### Layout

```
┌─────────────────────────────────────────────┐
│ Energy Tariffs                              │
│                                             │
│ Mode: [● Standard] [○ Flux] [○ Cosy] [○ Custom] │
│                                             │
│ ┌─ Standard mode (existing UI, unchanged) ─┐│
│ │ Import: peak / off-peak / times          ││
│ │ Export: peak / off-peak / times          ││
│ └───────────────────────────────────────────┘│
│                                             │
│ ┌─ Flux mode ──────────────────────────────┐│
│ │ Import                                   ││
│ │ ┌─────────┬─────────┬──────────────────┐ ││
│ │ │Off-peak │ Day     │ Peak             │ ││
│ │ │7.8p/kWh │26.5p/kWh│42.0p/kWh        │ ││
│ │ │02:00–05:│         │16:00–19:00       │ ││
│ │ └─────────┴─────────┴──────────────────┘ ││
│ │ Export                                   ││
│ │ ┌─────────┬─────────┬──────────────────┐ ││
│ │ │Off-peak │ Day     │ Peak             │ ││
│ │ │9.8p/kWh │15.0p/kWh│22.0p/kWh        │ ││
│ │ │02:00–05:│         │16:00–19:00       │ ││
│ │ └─────────┴─────────┴──────────────────┘ ││
│ └───────────────────────────────────────────┘│
│                                             │
│ ┌─ Cosy mode ─────────────────────────────┐│
│ │ Import                                   ││
│ │ ┌───────┬──────┬───────┬──────┬───────┐ ││
│ │ │Cosy 1 │ Day  │ Cosy 2│Peak  │Cosy 3 │ ││
│ │ │04-07  │07-13 │13-16  │16-19 │22-00  │ ││
│ │ └───────┴──────┴───────┴──────┴───────┘ ││
│ │ (no export for Cosy — uses flat rate)    ││
│ └───────────────────────────────────────────┘│
│                                             │
│ ┌─ Custom mode ───────────────────────────┐│
│ │ [+ Add Zone] (name, rate, start, end)   ││
│ │ Default rate: [____] £/kWh              ││
│ └───────────────────────────────────────────┘│
│                                             │
│ [Save Tariffs]                              │
└─────────────────────────────────────────────┘
```

#### Interaction

1. **Preset buttons** (Standard / Flux / Cosy / Custom):
   - Selecting a preset fills in the default zones and rates for that tariff
   - User can then tweak individual values
   - "Standard" shows the existing 2-zone UI (peak/off-peak)
   - "Custom" shows a generic zone editor with add/remove

2. **Switching modes**:
   - Standard → Flux: converts off-peak window to a zone, adds peak zone, sets day rate = old peak rate
   - Flux → Standard: loses the peak zone; warns user
   - Any → Custom: keeps current zones, makes them editable

3. **Visual rate timeline** (nice-to-have):
   - A small bar at the top of each tariff showing the 24h day colour-coded by rate
   - Helps users verify their zone times are correct
   - Can be a later iteration

### Phase 4 — Cost Calculation Engine (`HistoryPage.tsx`)

#### New rate-lookup function

Replace the `isOffPeak()` binary choice with a generic resolver:

```typescript
function getRate(
  ts: number,
  mode: TariffMode,
  standardCfg: TariffConfig,
  multiCfg: MultiZoneTariffConfig | null
): number {
  if (mode === 'multizone' && multiCfg) {
    return getMultiZoneRate(ts, multiCfg);
  }
  // Standard (existing logic)
  return isOffPeak(ts, standardCfg.off_peak_start, standardCfg.off_peak_end)
    ? standardCfg.off_peak_rate
    : standardCfg.peak_rate;
}

function getMultiZoneRate(ts: number, cfg: MultiZoneTariffConfig): number {
  const minutes = toMinutes(ts);
  for (const zone of cfg.zones) {
    if (inTimeWindow(minutes, zone.start, zone.end)) {
      return zone.rate;
    }
  }
  return cfg.default_rate; // "Day" rate
}

function inTimeWindow(minutes: number, start: string, end: string): boolean {
  const [sh, sm] = start.split(':').map(Number);
  const [eh, em] = end.split(':').map(Number);
  const startMins = sh * 60 + sm;
  const endMins = eh * 60 + em;
  if (startMins <= endMins) {
    return minutes >= startMins && minutes < endMins;
  }
  return minutes >= startMins || minutes < endMins;
}
```

#### Updated preprocess functions

The existing cost `preprocess` functions change minimally — only the rate lookup line:

```typescript
// Before (standard only):
const rate = isOffPeak(row.t, importTariffCfg.off_peak_start, importTariffCfg.off_peak_end)
  ? importTariffCfg.off_peak_rate : importTariffCfg.peak_rate;

// After (both modes):
const rate = getRate(row.t, tariffMode, importTariffCfg, multizoneImportCfg);
```

**Everything else stays the same** — delta computation, midnight rollover handling, spike clamping, accumulation. The cost engine is unchanged; only the rate-at-time-t function changes.

### Phase 5 — Enhanced Cost Tab (Optional / Phase 2)

When in multizone mode, the cost tab can show richer information:

#### Option A: Coloured stacked areas

Instead of a single "Import Cost" line, show stacked areas per zone:

```
Import Cost (£)
  ▓▓▓▓▓▓▓▓▓▓▓  <- Peak cost (red)
  ████████████  <- Day cost (orange)
  ░░░░░░░░░░░  <- Off-peak cost (blue)
```

#### Option B: Zone breakdown summary

Below the existing charts, add a summary card:

```
Today's Cost Breakdown
┌──────────────┬────────┬──────────┬──────────┐
│              │ kWh    │ Rate      │ Cost     │
├──────────────┼────────┼──────────┼──────────┤
│ Off-peak imp │  3.2   │ 7.8p     │ £0.25    │
│ Day import   │  8.1   │ 26.5p    │ £2.15    │
│ Peak import  │  4.5   │ 42.0p    │ £1.89    │
│              │        │          │          │
│ Off-peak exp │  0.8   │ 9.8p     │ £0.08    │
│ Day export   │  5.2   │ 15.0p    │ £0.78    │
│ Peak export  │  6.8   │ 22.0p    │ £1.50    │
│              │        │          │          │
│ Net cost     │        │          │ £1.93    │
└──────────────┴────────┴──────────┴──────────┘
```

This requires tracking per-zone kWh deltas separately (in addition to the running £ total). The preprocess function would emit multiple synthetic fields:

```typescript
// In multizone mode, preprocess emits:
{
  ...row,
  _import_cost: acc,                    // total running cost
  _import_cost_offpeak: offpeakAcc,     // running off-peak cost
  _import_cost_day: dayAcc,             // running day cost
  _import_cost_peak: peakAcc,           // running peak cost
}
```

This is additive — the existing `_import_cost` and `_export_income` fields still work for the standard chart. The zone breakdown is bonus data.

---

## Implementation Order

### Step 1: Backend data model + API (non-breaking)

1. Add `TariffZone`, `MultiZoneTariffConfig` structs to `settings/mod.rs`
2. Add `tariff_mode`, `multizone_import_config`, `multizone_export_config` to `Settings` with `#[serde(default)]`
3. Add preset factory functions (`flux_import()`, `flux_export()`, `cosy_import()`)
4. Expose new fields in `GET /api/settings` and `PUT /api/settings`
5. Add unit tests for serialization roundtrip and zone overlap validation
6. Run `cargo clippy` + `cargo test`

**Non-breaking**: All existing settings files load unchanged. New fields default to `"standard"` / `None`.

### Step 2: Frontend types + state

1. Add `TariffZone`, `MultiZoneTariffConfig`, `TariffMode` types to `src/lib/types.ts`
2. Add fields to `PollSettings` type
3. Update `HistoryPage.tsx` state to hold multizone configs (loaded from settings API)
4. `npm run build` passes

### Step 3: Settings UI

1. Add mode selector (Standard / Flux / Cosy / Custom) to `SettingsPage.tsx`
2. Implement Flux preset UI (3 zones, pre-filled times, editable rates)
3. Implement Cosy preset UI (4 zones, pre-filled times, editable rates)
4. Implement Custom mode (add/remove zones freely)
5. Wire mode selection to settings save API
6. Add mode-switching logic (Standard → Flux conversion, etc.)
7. `npm run lint` + `npm run build`

### Step 4: Cost calculation engine

1. Add `getRate()` and `getMultiZoneRate()` functions to `HistoryPage.tsx`
2. Update cost `preprocess` functions to use `getRate()` instead of `isOffPeak()`
3. Verify standard mode still produces identical results (regression test by eye)
4. Verify Flux mode produces correct 3-zone cost curves
5. `npm run lint` + `npm run build`

### Step 5: Enhanced cost tab (Phase 2 / nice-to-have)

1. Add per-zone accumulation in preprocess functions
2. Add zone breakdown summary card below charts
3. Optionally add coloured stacked area chart
4. Add 24h rate timeline visualisation in settings

### Step 6: Testing + verification

1. `cargo clippy` + `cargo test` (Rust)
2. `npm run lint` + `npm run build` (TypeScript)
3. Manual testing: Standard mode cost graphs unchanged
4. Manual testing: Flux mode cost graphs show 3-zone rates
5. Manual testing: Switching modes preserves data correctly
6. Settings roundtrip: save, restart, verify loaded correctly

---

## Risk Analysis

| Risk | Mitigation |
|---|---|
| **Existing cost graphs break** | `tariff_mode` defaults to `"standard"`, zero code path change for existing users |
| **Zone overlap causes double-counting** | Validate no overlaps on save; UI highlights conflicting zones |
| **Midnight-crossing zones** | Reuse existing `isOffPeak` midnight logic in `inTimeWindow` |
| **Users confused by preset vs custom** | Clear labels, "Flux (pre-filled, editable)" not "Flux (locked)" |
| **Future tariffs with different zone counts** | Generic N-zone model handles 1–N zones naturally |
| **Rate accuracy** | Rates are user-editable; presets are defaults only, not live API data |
| **Settings migration on upgrade** | New fields have `#[serde(default)]`; old settings files load cleanly |

---

## What This Does NOT Cover

- **Live rate fetching** from Octopus API — rates are user-entered (could be a future enhancement)
- **Agile half-hourly rates** — already handled by the existing Agile mode (separate feature)
- **Tariff automation** — this is purely cost calculation/accounting for the history page
- **Standing charges** — could be added as a flat daily charge in the summary, but out of scope for this issue

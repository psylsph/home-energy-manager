# Roadmap

Planned and under-investigation items for Home Energy Manager. This is not a
release commitment; items may change as hardware access, simulator support, and
user reports improve.

## Power page CSV/PDF exports and period summaries

**Status**: In progress.

Users have requested export from the **Power** panel specifically. This is the
combined view that overlays all key feeds in one place, not the individual
History tabs. Export output should be based on the same selected Power range
(`1h`, `6h`, `12h`, `24h`, `Today`, `7d`, `30d`, `Month`, `6m`, `1y`) and should
include both detailed samples and useful period summaries.

### Export scope

The Power panel currently combines:

- Combined PV generation (`solar_power` → displayed as positive W)
- Battery power (`battery_power` → inverted so positive means discharge and
  negative means charge)
- Grid power (`grid_power` → inverted so positive means import and negative
  means export)
- Home/load power (`home_power` → displayed as positive W)
- Battery state of charge (`soc`)

Exports should use these transformed display semantics so CSV/PDF values match
what users see on the Power chart.

### CSV requirements

CSV export should include three sections:

1. **Report metadata and summary totals**
   - Report name, selected period label, generated timestamp
   - Total solar generation kWh
   - Total home/load kWh
   - Total grid import kWh
   - Total grid export kWh
   - Net grid kWh (`import - export`)
   - Battery charged kWh
   - Battery discharged kWh
   - Peak solar W, peak home W, peak grid import/export W
   - Minimum, maximum, and average SOC
   - Solar coverage percentage (`solar kWh / home kWh`)
   - Grid dependency percentage (`import kWh / home kWh`)

2. **Bucketed breakdown for charting/spreadsheets**
   - Hourly buckets for sub-day and `Today` ranges
   - Daily buckets for `7d`, `30d`, and `Month`
   - Monthly buckets for `6m` and `1y`
   - Columns: bucket label, solar kWh, home kWh, import kWh, export kWh,
     battery charge kWh, battery discharge kWh, min/avg/max SOC

3. **Detailed transformed samples**
   - Timestamp ISO, local timestamp
   - Solar W
   - Battery W and direction (`Charging`, `Discharging`, `Idle`)
   - Grid W and direction (`Importing`, `Exporting`, `Idle`)
   - Home/load W
   - SOC %

CSV exports should include all Power series for the selected range, regardless
of legend-muted visual state. Muting is a chart display preference, not a data
filter.

### PDF requirements

PDF output should be a polished printable Power report using the same selected
range and transformed data. The first implementation can use a printable report
window (`window.print()` / save as PDF) to avoid a heavy PDF dependency; a direct
PDF writer can be added later if required.

Suggested layout:

1. **Summary dashboard**
   - Header: Home Energy Manager, Power Report, selected period, generated time
   - KPI cards for solar generated, home consumed, grid imported, grid exported,
     net grid, battery charged/discharged, solar coverage, grid dependency,
     peak load, and SOC range

2. **Combined Power view**
   - A chart equivalent to the Power panel: PV, battery charge/discharge, grid
     import/export, home/load, and SOC context

3. **Period breakdown bar charts**
   - For `Month`, show a bar per day
   - For `7d`/`30d`, show daily bars
   - For `6m`/`1y`, show monthly bars
   - For sub-day/Today, show hourly bars
   - Use separate, readable charts rather than a crowded single chart:
     - Solar vs home/load
     - Grid import vs export
     - Battery charge vs discharge

4. **Pie/donut charts**
   - Grid balance: import vs export
   - Battery activity: charge vs discharge
   - Estimated solar destination: used locally, charged to battery, exported
     (clearly labelled as estimated where exact routing is not available)

5. **Highlights and table**
   - Best solar bucket/day
   - Highest home/load bucket/day
   - Highest import bucket/day
   - Highest export bucket/day
   - Lowest SOC bucket/day
   - Compact bucket table with kWh and SOC values

### Implementation detail

- Keep implementation frontend-only initially in `src/pages/PowerPage.tsx`.
- Reuse the existing Power history fetch and transformed `PowerRow` model.
- Add `calculatePowerReport()` helper that integrates displayed power samples
  over time to estimate kWh totals for the selected period.
- Skip unusually large gaps when integrating so offline periods do not create
  inflated totals.
- Add bucket aggregation helper with bucket size selected from the active range.
- Add `exportPowerCSV()` for metadata, bucket rows, and detailed sample rows.
- Add `exportPowerPDF()` that creates a printable report window with inline CSS
  and lightweight SVG bar/pie charts.
- Preserve Power page chart behaviour and avoid changing Modbus/backend history
  storage for this first iteration.

### Future improvements

- Offer “summary only” vs “detailed” PDF modes.
- Add direct PDF generation if print-to-PDF is not acceptable.
- Optionally include cost summaries using import/export tariff settings.
- Consider exact cumulative-counter totals where available, while keeping the
  displayed Power-row totals as the visible-data baseline.
- Add scheduled monthly report generation once manual exports settle.

---

### HV battery capacity — nominal vs usable

**Status**: Data available but frontend displays nominal, not usable.

HV stackable batteries (GIV-BAT-3.4-HV modules) report capacity via the BCU
cluster at device 0x70, IR(98) per-module Ah × IR(64) module count × 76.8V
nominal ÷ 1000.

| Source | Value for 5-module stack | Notes |
|---|---|---|
| BCU IR(98) per module | 51.0 Ah | deci: raw 510 ÷ 10 |
| BCU IR(64) modules | 5 | |
| Total Ah | 255 Ah | 51 × 5 |
| Nominal kWh | **~19.6 kWh** | 255 × 76.8 ÷ 1000 |
| Minus 10% overhead | ~17.6 kWh | GivTCP's 0.9 factor |
| Minus 4% SOC reserve | ~16.9 kWh | Default reserve |
| Minus 10% SOC reserve | ~15.8 kWh | User-configurable |

**Gap**: The displayed capacity is the raw nominal from the BCU (~20 kWh),
not the nameplate rating (~17 kWh for GIV-BAT-17.0-HV). GivTCP applies a
0.9 factor (`battery_capacity_hv` converter). The app should either:

- Apply the 0.9 factor to match the datasheet, or
- Display both nominal and usable, or
- Document that the value is theoretical and usable depends on reserve.

See also: `derive_three_phase_battery_fields()` in poll.rs, GivTCP
`register.py:battery_capacity_hv()`.

## Near-term candidates

### Multi-Zone Tariff Cost Calculations (Issue #64 Part A)

**Status**: Planning complete. Related to [Issue #64](https://github.com/psylsph/home-energy-manager/issues/64).

The current `TariffConfig` is a 2-zone model (peak + off-peak) that cannot
represent tariffs like Octopus Flux (3 zones) or Cosy (4+ zones). This project
will implement a generic N-zone tariff system to accurately compute costs on
the History page.

#### Octopus Flux zone structure

Octopus Flux is a 3-rate import+export tariff for solar & battery owners:

| Zone | Time window | Import rate | Export rate | Notes |
|---|---|---|---|---|
| Off-peak | 02:00–05:00 | Cheapest | Standard | Charge battery from grid |
| Day rate | everything else | Standard | Standard | Default/fallback |
| Peak | 16:00–19:00 | Most expensive | **Highest** | Discharge/export to grid |

Key characteristics:

- Fixed zones (not half-hourly like Agile) — set once, applies every day
- 14 UK regions (GSP groups A–P, excluding I/O) with region-specific rates
- Symmetrical import/export structure (3 zones each, same time windows)
- Export rates differ from import — peak export is particularly lucrative
- No authentication required for public tariff rate lookups via Octopus API
- Temporarily unavailable for new signups (as of June 2026) but existing
  customers still on the tariff

#### Technical Design

**Data model** (replaces current `TariffConfig`):

```rust
/// A single time-bounded rate zone.
pub struct TariffZone {
    /// Human-readable label (e.g. "Off-peak", "Peak", "Day rate").
    pub label: String,
    /// Rate in £/kWh during this zone.
    pub rate: f64,
    /// Zone start time in "HH:MM" format (24h).
    pub start: String,
    /// Zone end time in "HH:MM" format (24h), exclusive.
    /// Can be before `start` to indicate crossing midnight.
    pub end: String,
}

/// Generic N-zone tariff configuration.
pub struct MultiZoneTariffConfig {
    /// Ordered list of rate zones.
    pub zones: Vec<TariffZone>,
    /// Default day rate (£/kWh) when no zone matches.
    pub default_rate: f64,
}
```

**Tariff presets** (constructors on `MultiZoneTariffConfig`):

| Preset | Import zones | Export zones |
|---|---|---|
| Standard (legacy) | Peak rate only (no zones, `default_rate` = user's peak rate) | Single export rate |
| Flux | 3 zones: off-peak 02:00–05:00, peak 16:00–19:00, day default | 3 zones matching import windows but with export rates |
| Cosy | User-defined (up to 3 slots, matching existing `CosySlot` windows) | Single export rate |
| Custom | User-defined N zones | User-defined N zones |

**Backward compatibility**:

- Old `TariffConfig` (`peak_rate`, `off_peak_rate`, `off_peak_start`, `off_peak_end`)
  in existing `settings.json` must deserialize into the new format without error
- Migration strategy: on `Settings::load()`, if `import_tariff_config` is the old
  `TariffConfig` shape (has `peak_rate`/`off_peak_rate` keys), convert to
  `MultiZoneTariffConfig` with a single off-peak zone + default_rate = peak_rate
- The old `import_tariff`/`export_tariff` flat fields remain as legacy fallback
  (already handled this way in the frontend)

**Rate resolution** (replaces `isOffPeak()`):

```typescript
function getRate(timestamp: number, config: MultiZoneTariffConfig): number {
  const minutes = getMinutesOfDay(timestamp);
  for (const zone of config.zones) {
    if (isInZone(minutes, zone.start, zone.end)) {
      return zone.rate;
    }
  }
  return config.default_rate;
}
```

- Zone matching is exclusive on `[start, end)` (matching existing `isOffPeak` semantics)
- Zones are evaluated in order; first match wins
- Zones crossing midnight (`end < start`) match `minutes >= start || minutes < end`

#### Files to change

| Layer | File | Change |
|---|---|---|
| Backend model | `src-tauri/src/settings/mod.rs` | Add `TariffZone`, `MultiZoneTariffConfig`. Replace `TariffConfig` with new type. Add preset constructors for Flux, Cosy, legacy 2-zone. Backward-compatible deserialization for existing `settings.json`. |
| Backend tests | `src-tauri/src/settings/mod.rs` | Add serde roundtrip tests for new types. Add migration tests: old `TariffConfig` JSON → new `MultiZoneTariffConfig`. Verify existing `settings_roundtrip` test passes unchanged. |
| Backend API | `src-tauri/src/server/api.rs` + `mod.rs` | Update `GET/PUT /api/settings` to serialize/deserialize new tariff format. |
| Frontend types | `src/lib/types.ts` | Replace `TariffConfig` with `MultiZoneTariffConfig` + `TariffZone`. Update `PollSettings`. |
| Frontend settings | `src/pages/SettingsPage.tsx` | Add tariff preset dropdown (Standard/Flux/Cosy/Custom) + N-zone editor UI. Preset switches pre-fill zones. |
| Frontend history | `src/pages/HistoryPage.tsx` | Replace `isOffPeak()` with generic `getRate(timestamp, config)`. Update cost preprocessing pipeline in cost tab chart definitions. |

#### Implementation order

1. **Backend settings model** — `TariffZone`, `MultiZoneTariffConfig`, preset constructors, backward-compat migration on load
2. **Backend serde tests** — verify old 2-zone JSON deserializes cleanly into new format, roundtrip new format, preset correctness
3. **Backend API** — update `GET/PUT /api/settings` endpoints
4. **Frontend types** — update `types.ts`
5. **Frontend settings UI** — preset dropdown + zone editor in `SettingsPage.tsx`
6. **Frontend history engine** — `getRate()` replaces `isOffPeak()`, update cost chart preprocessing
7. **Optional: Octopus API integration** — fetch live Flux rates per region for auto-populating zone rates (instead of manual entry)

#### Key design constraints

- Existing `settings.json` with old `TariffConfig` must load without error (migration on read, not write)
- Flux preset zones: off-peak 02:00–05:00, peak 16:00–19:00, day rate default
- Flux export preset: same windows but with export-specific rates
- Day rate = `default_rate` (fallback when no zone matches the timestamp)
- Zone matching is exclusive on `[start, end)` (matching existing `isOffPeak` semantics)
- No database schema changes — tariffs are settings-only
- No Modbus register writes — this is purely a display/cost-calculation feature

---

### Eco/Echo Mode Clarification (Issue #64 Part B)

**Status**: Planning complete. Related to [Issue #64](https://github.com/psylsph/home-energy-manager/issues/64).

#### Background

The issue reporter refers to "Echo mode" — this is **Eco mode** in the
Modbus protocol (HR(27) = `battery_power_mode`: 0 = export, 1 = self-consumption).
The GivEnergy cloud portal labels this "Eco Mode" but the user may have seen
"Echo" in a newer portal version or may be misremembering the name.

The user is confused by the behaviour difference between HEM and the GivEnergy
cloud:

- **GivEnergy cloud**: Eco mode is always "enabled" as a background overlay.
  Charge/discharge schedules coexist with Eco mode.
- **HEM**: Switching to Eco mode clears discharge slot registers on the inverter.
  Discharge schedules are held client-side only and restored when switching
  back to Timed mode.

#### Why HEM clears registers

This is a **deliberate safety feature**, not a bug. Gen3 firmware has a quirk
where non-zero discharge slot registers can auto-enable `enable_discharge`,
causing unrestricted export. HEM clears registers to prevent this. See
`AGENTS.md` → "Discharge slot handling" → "Eco mode constraints" for full
details.

This behaviour must **not** be changed — it prevents real safety issues on
Gen3 hardware.

#### Proposed changes

| Layer | File | Change |
|---|---|---|
| Frontend | `src/pages/ControlPage.tsx` | Add "(also known as Echo mode)" parenthetical on the Eco mode button tooltip/label |
| Frontend | `src/pages/ControlPage.tsx` | Expand the existing Eco mode tooltip to explain: battery supplies home first, discharge schedules are held client-side in Eco and restored on Timed switch |
| Frontend | `src/pages/ControlPage.tsx` | Add a yellow info banner (matching the existing slot-ordering banner style) below the mode selector explaining the Eco vs Timed discharge schedule handling |

**NOT changing**:

- The register-clearing behaviour when entering Eco mode (safety feature)
- The client-side discharge slot holding logic
- Any Modbus writes
- The `BatteryMode` enum or derivation logic

#### Register reference

From the reference libraries:

- **givenergy-modbus**: `battery_power_mode = Def(C.uint16, BatteryPowerMode, HR(27))` where `BatteryPowerMode::EXPORT = 0`, `BatteryPowerMode::SELF_CONSUMPTION = 1`
- **GivTCP**: `eco_mode = Def(C.uint16, Enable, HR(27), valid=(0, 1))` — same register, same semantics, different naming (`eco_mode` vs `battery_power_mode`)
- **GivTCP**: `set_eco_mode(enabled)` writes HR(27) to 1 (eco/self-consumption) or 0 (export)
- **HEM**: `BatteryMode::from_registers(battery_power_mode, enable_discharge, battery_soc_reserve)` derives the displayed mode from HR(27) + HR(59) + HR(110)

---

### Flux-Aware Auto-Scheduling (Issue #64 Part C, future)

**Status**: Future consideration. Related to [Issue #64](https://github.com/psylsph/home-energy-manager/issues/64).

The user asks: "It would be nice to have the cal's take account of the off
peak charge time and the peak avoid period." This goes beyond cost display to
**automated charging/discharging based on tariff zones**.

This is essentially a Flux-specific version of the existing Cosy charging mode
and should only be attempted after the multi-zone tariff system (Part A) is
complete.

#### Proposed design

- Add **"Flux"** as a 4th charging mode option in the dropdown: Standard / Cosy / Agile / **Flux**
- Pre-configured charge slot: 02:00–05:00 (force-charge from grid at off-peak rate)
- Pre-configured discharge slot: 16:00–19:00 (force-discharge/export at peak rate)
- Uses the existing Cosy-style state machine (`ForceCharge`/`CosyExit`) but with
  Flux-specific time windows derived from the tariff config
- Settings: `flux_enabled` bool, inherits zone times from the multi-zone tariff config

#### Implementation prerequisites

1. Multi-zone tariff system (Part A) must be complete
2. Tariff zone times must be accessible to the poll loop for scheduling decisions
3. New `flux_enabled` / `flux_active` settings fields + persistence (same pattern as `cosy_enabled` / `cosy_active_persisted`)
4. New state machine in `poll.rs` alongside existing Cosy/Agile logic

#### Risk assessment

- Low risk: follows the exact same architecture as Cosy mode (well-tested pattern)
- Flux zones are fixed daily (no API calls needed for scheduling, unlike Agile)
- Main risk is interaction between Flux/Cosy/Agile/Winter modes — mutual exclusion
  logic already exists (`auto_winter_active || cosy_active || agile_active` check
  in poll.rs); Flux would join the same guard

---

### Static asset caching headers

**Status**: Planned. Related to [Issue #59](https://github.com/psylsph/home-energy-manager/issues/59).

The F5-refresh problem is solved by switching to `HashRouter` (v0.17.10),
but a secondary improvement is to add proper `Cache-Control` headers to the
Axum static file serving in `src-tauri/src/server/mod.rs`.

Currently `tower_http::services::ServeDir` serves all files (including
`index.html`) with no cache directives. After an app update, browsers may
serve a stale `index.html` that references old content-hashed JS bundles,
causing a blank page until the cache expires (reported as "up to 10 minutes").

**Proposed change**:

1. Enable the `set-header` feature on `tower-http` in `Cargo.toml`:

   ```toml
   tower-http = { version = "0.6", features = ["cors", "fs", "set-header"] }
   ```

2. In `create_router_with_frontend`, wrap `ServeDir` with response header
   layers:
   - `Cache-Control: public, max-age=31536000, immutable` on everything
     under `/assets/` — Vite content-hashes filenames so they're safe to
     cache forever
   - `Cache-Control: no-store` on `index.html` — the SPA entry point must
     never be cached because it changes with every deploy

3. Consider replacing the double `ServeDir` fallback with an explicit
   catch-all route for the SPA that serves `index.html` with `no-store`,
   while letting `ServeDir` handle `/assets/*` with aggressive caching.

This is standard SPA deployment practice (same pattern as nginx/Caddy/Vercel).

### GivCloud DNS re-configuration (dongle-level)

**Status**: Under investigation. Related to [giv_tcp issue #546](https://github.com/britkat1980/giv_tcp/issues/546).

If GivEnergy Ltd were to go under, the `givenergy.cloud` domain could be sold to an untrusted third party, potentially giving them control over all customer installations — even those using local control.

#### Discovery

The GivEnergy dongle exposes a plaintext configuration protocol on **telnet port 23**. Using `netcat`:

```
$ nc 192.168.X.XXX 23
Login as:admin
Password:admin
CMD>cfg
CFG>prof show
#PROFILE
#VER_2_1
...
M2M_NET2_ENABLE=1
M2M_NET2_PORT=7654
M2M_NET2_SERADD=comms.givenergy.cloud
M2M_NET2_TCPTO=300
...
```

The `M2M_NET2_*` settings control the dongle's connection to the GivEnergy cloud.

#### Interaction commands

| Command | Description |
|---|---|
| `up` | Navigate up the menu hierarchy |
| `cfg > set M2M_NET2_SERADD <addr>` | Change the cloud server address |
| `cfg > prof save` | Persist changes to flash storage |
| `reboot` | Reboot dongle to apply config changes |

#### Safety notes

- Do **not** just set `M2M_NET2_ENABLE` to `0` — on boot, the inverter re-enables it
- Recommended sandbox address: `127.0.0.1` (prevents any outbound cloud traffic)
- The dongle emits a lot of debug junk; response traffic needs filtering

#### Proposed implementation

1. **Backend**: Add a lightweight telnet/CLI client to interact with the dongle's config shell — parse `prof show` output to extract current M2M settings, send `set` commands to override, and trigger `prof save` + `reboot`
2. **API**: `GET /api/dongle/cloud-config` (read current M2M settings), `POST /api/dongle/cloud-config` (update server address)
3. **Frontend**: Settings page section — show current dongle cloud address with an override input (default `127.0.0.1`) and a "Reboot dongle" button
4. **Advanced**: For users running a local GivTCP server, allow setting the address to their own server (enables fully local-cloud alternatives like Axle or Predbat)

### Octopus Agile Integration

**Status**: Investigation complete; implementation not started.

[Issue #50](https://github.com/psylsph/home-energy-manager/issues/50) requests
support for Octopus Agile tariff. The Octopus Energy REST API is publicly
accessible without authentication — no Octopus account needed.

#### API findings

| Endpoint | Description |
|---|---|
| `GET /v1/products/` | List available tariffs — `AGILE-24-10-01` (import) and `AGILE-OUTGOING-19-05-13` (export) |
| `GET /v1/products/{code}/` | Tariff metadata, standing charges, links to unit rates |
| `GET /v1/products/{code}/electricity-tariffs/{rate_code}/standard-unit-rates/` | Half-hourly prices with `valid_from`/`valid_to` and `value_inc_vat` (pence/kWh) |

Key characteristics:

- **No authentication required** for tariff rate lookups
- **Half-hourly granularity** (48 slots/day)
- **Day-ahead prices** published ~4pm BST each day
- **Prices can go negative** (get paid to consume)
- **Capped** at 100p/kWh (inc VAT)
- **14 UK regions** (GSP group codes `_A` through `_P`, excluding `I`/`O`)
- **Export variant**: Agile Outgoing (`AGILE-OUTGOING-19-05-13`) also available

#### Proposed scope

**Architecture**: Follow the existing Cosy charging pattern — a state machine
in the poll loop that triggers `ForceCharge`/`CosyExit` when conditions are
met. Instead of fixed user-defined time slots, the decision is based on the
current half-hour Agile price vs a user-configured threshold.

**Backend** — optionally new module `src-tauri/src/octopus/`:

| File | Purpose |
|---|---|
| `client.rs` | HTTP client for Octopus API, fetches/parses unit rates, handles pagination and caching |
| (or inline in `poll.rs`) | Price-check logic: current price below threshold → force-charge, above threshold → discharge (if price > threshold + margin) |

**New/changed state in AppState / Settings**:

| Field | Type | Default |
|---|---|---|
| `agile_enabled` | bool | `false` |
| `agile_region` | GSP group code | `_A` (Eastern England) |
| `agile_charge_threshold` | f64 (pence/kWh) | `10.0` |
| `agile_discharge_threshold` | f64 (pence/kWh) | `30.0` |
| `agile_cached_prices` | Vec of price slots | empty (refreshed each hour) |

**New API endpoints**:

| Method | Endpoint | Description |
|---|---|---|
| GET | `/api/agile` | Current config, current price, next few upcoming prices |
| POST | `/api/agile` | Update Agile config (enabled, region, thresholds) |

**Frontend**:

- Add `'agile'` option to the Charging Mode dropdown (Standard / Cosy / Agile)
- When Agile is selected, show Agile-specific controls instead of Cosy slot editors:
  - Charge threshold slider (e.g., 0–50p/kWh) with current price indicator
  - Discharge threshold slider
  - Region selector (if configurable)
  - Price preview: next 4–8 half-hours with bars showing price vs threshold

**Poll loop logic**:

Same pattern as Cosy — on each poll cycle:

1. If `agile_enabled` and prices are cached for current time:
   - Get current price from cache
   - If price ≤ `agile_charge_threshold` AND not already charging → `ForceCharge`
   - If price ≥ `agile_discharge_threshold` AND currently charging → `CosyExit` (restore Eco)
   - Optionally: if price ≥ `agile_discharge_threshold` AND battery SOC > reserve → `ForceDischarge`
2. If cache is stale (no price data for current 30-min slot) → fetch from Octopus API
3. Refresh cache every hour or on demand

#### Implementation order

1. Backend: Octopus API client — fetch and cache prices
2. Backend: Extend `AppState` with Agile state + settings
3. Backend: `GET /api/agile` and `POST /api/agile` endpoints
4. Backend: Agile state machine in poll loop (alongside Cosy)
5. Frontend: Add `'agile'` to Charging Mode dropdown
6. Frontend: Agile controls section (thresholds, price preview)

#### Reference

- Octopus API: `https://api.octopus.energy/v1/` (public, no auth)
- Developer docs: `https://developer.octopus.energy/`
- GSP group → region mapping: [Wikipedia — GSP Group](https://en.wikipedia.org/wiki/Grid_Supply_Point)

## Later candidates

### Read-only EMS support

EMS support should be treated separately from normal inverter polling. Initial
support should be read-only until real hardware or simulator coverage is
available.

Known information from previous investigation:

- EMS uses device address `0x11`
- EMS config block: holding registers `2040..2075`
- EMS runtime block: input registers `2040..2094`
- EMS model prefixes: `5` / `51`

### AC Three-Phase (AC3 / DTC 0x60xx) — already supported

The AC Three-Phase inverter (DTC `0x6001–0x60ff`) is already fully implemented
in HEM. It shares the same register layout and polling path as the three-phase
hybrid models (`HybridHvGen3`, `AllInOneHybrid`, `ThreePhase`).

**What works today:**

- `needs_three_phase_input_blocks()` → true (IR 1000-1413 polled and decoded)
- `uses_three_phase_schedule_slots()` → true (10 slots at HR 1113-1121 + HR 240-299)
- `uses_hv_battery()` → true (BCU/BMU protocol at 0x70/0x50)
- `extra_poll_blocks()` → `AC_EXTENDED_AND_THREE_PHASE_BLOCKS` (AC config +
  extended slots + three-phase config)
- Decoder handles three-phase input register blocks (IR 1000-1413) including
  per-phase voltage/current/power, PV, battery, and energy totals
- Encoder has model-specific write targets for three-phase (HR 1108/1109/1110/1111)
- Charge/discharge rate scaling uses the AC-coupled 1-100% range (not the DC
  hybrid 0-50% range)
- Export priority (HR 311), EPS enable (HR 317), AC charge/discharge limits
  (HR 313/314) are all decoded and writable
- `preferred_read_slave_address()` → 0x11

**Reference library mapping:**

| Reference | Model | DTC |
|---|---|---|
| givenergy-modbus `Model.AC_3PH` | `"6"` (coarse family) | 0x60xx |
| GivTCP `Model.AC_3PH` | `"60"` | 0x6001 |
| HEM `DeviceType::ACThreePhase` | — | 0x6001-0x60ff |

The AC3 uses the same three-phase register layout defined in
`givenergy_modbus/model/inverter_threephase.py`. Slots 1-2 are at HR 1113-1121
(shadowing the single-phase HR 94-95 / 31-32 / 56-57 / 44-45), slots 3-10 at
HR 240-299. Battery limits use HR 1108 (discharge) and HR 1110 (charge) with
1-100% register range. Per givenergy-modbus, AC_3PH is in `AC_COUPLED_MODELS`
(alongside single-phase `Model.AC`) and in `THREE_PHASE_MODELS`.

**No further work needed** unless a user reports model-specific issues.

---

### Gateway (DTC 0x70xx) — not yet supported

**Status**: Investigation complete; implementation not started.

The GivEnergy Gateway is a **system controller** that manages up to 3
All-in-One (AIO) battery units in parallel. It is not an inverter itself — it
is a coordination hub that aggregates data from the attached AIOs. Identified by
DTC range `0x7001–0x70ff`.

#### Device overview

| Property | Value |
|---|---|
| DTC range | `0x7001–0x70ff` |
| Reference model | `givenergy-modbus` `Model.GATEWAY` (`"7"`), GivTCP `Model.GATEWAY` (`"70"`) |
| HEM `DeviceType` | `Gateway` |
| Slave address | 0x11 (detection + operational) |
| Max AC power | 12,000 W (hardcoded in HEM) |
| Battery power | 6,000 W × `parallel_aio_num` (per GivTCP) |
| Battery capacity | 13.5 kWh × `parallel_aio_num` (per GivTCP) |
| Schedule slots | N/A — Gateway delegates to AIOs |
| Max AIO units | 3 (per-AIO SOC, power, energy, serial number) |

#### Current HEM support (stub only)

The Gateway is recognised at the `DeviceType` level but has no meaningful
polling, decoding, or control support:

| Aspect | Current state |
|---|---|
| `DeviceType::from_register()` | ✅ Maps 0x70xx → `Gateway` |
| `display_name()` | ✅ Returns `"Gateway"` |
| `max_battery_power_w()` | Returns 0 (should be `6000 × aio_count`) |
| `max_ac_power_w()` | Returns 12000 (hardcoded) |
| `extra_poll_blocks()` | Returns `&[]` (no extra blocks polled) |
| `supports_schedule_slots()` | Returns `false` (correct) |
| `needs_three_phase_input_blocks()` | Returns `false` |
| `uses_hv_battery()` | Returns `false` |
| `preferred_read_slave_address()` | Returns `0x11` |
| Gateway IR/HR decoder | ❌ None — IR 1600+ range not handled |
| Gateway-specific snapshot fields | ❌ None |
| Write/encoder support | ❌ None (treated like EMS/Gateway in exclusion set) |

The standard poll blocks (IR 0-59, HR 0-59, HR 60-119) are read for the
Gateway at slave 0x11, but these contain only the identity registers
(serial number, firmware). The Gateway's live measurements and configuration
live in completely different register ranges (see below).

#### Gateway register layout

The Gateway uses a unique register address space, distinct from all other
GivEnergy devices. Data is sourced from `givenergy-modbus/model/gateway.py`
and GivTCP `givenergy_modbus_async/model/register.py`.

##### Input registers — live measurements (IR 1600-1859)

Per `givenergy-modbus` `refresh()`: reads IR 1600-1859 in 60-register chunks.

**IR 1600-1631 — System state:**

| Registers | Field | Converter | Notes |
|---|---|---|---|
| IR(1600-1603) | `software_version` | `gateway_version` | 4-register Latin-1 string (e.g. "GA000010") |
| IR(1604) | `work_mode` | uint16 → WorkMode enum | |
| IR(1608) | `v_grid` | int16 / deci | Grid voltage (V) |
| IR(1609) | `i_grid` | int16 / deci | Grid current (A) |
| IR(1610) | `v_load` | deci | Load voltage (V) |
| IR(1611) | `i_load` | deci | Load current (A) |
| IR(1612) | `i_pv` | int16 / deci | PV current (A) |
| IR(1616) | `p_ac1` | int16 | AC power 1 (W) |
| IR(1617) | `p_pv` | uint16 | PV power (W) |
| IR(1618) | `p_load` | uint16 | Load power (W) |
| IR(1619) | `p_liberty` | int16 | Liberty power (W) |
| IR(1620-1621) | `fault_protection` | uint32 | 32-bit fault bitmask |
| IR(1622-1623) | `gateway_fault_codes` | uint32 → decoded | 32-bit fault bitmask, MSB-first decode |
| IR(1624) | `v_grid_relay` | deci | Grid relay voltage |
| IR(1625) | `v_inverter_relay` | deci | Inverter relay voltage |
| IR(1627-1631) | `first_inverter_serial_number` | serial | 5-register serial |

**IR 1640-1657 — Daily/today energy totals:**

| Registers | Field | Converter | Notes |
|---|---|---|---|
| IR(1640) | `e_grid_import_today` | deci | kWh today |
| IR(1641-1642) | `e_grid_import_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1643) | `e_pv_today` | deci | kWh today |
| IR(1644-1645) | `e_pv_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1646) | `e_grid_export_today` | deci | kWh today |
| IR(1647-1648) | `e_grid_export_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1649) | `e_aio_charge_today` | deci | kWh today |
| IR(1650-1651) | `e_aio_charge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1652) | `e_aio_discharge_today` | deci | kWh today |
| IR(1653-1654) | `e_aio_discharge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1655) | `e_load_today` | deci | kWh today |
| IR(1656-1657) | `e_load_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |

**IR 1700-1758 — Per-AIO summary and energy:**

| Registers | Field | Converter | Notes |
|---|---|---|---|
| IR(1700) | `parallel_aio_num` | uint16 | Total AIO count |
| IR(1701) | `parallel_aio_online_num` | uint16 | Online AIO count |
| IR(1702) | `p_aio_total` | int16 | Total AIO power (W) |
| IR(1703) | `aio_state` | uint16 → State | Battery state enum |
| IR(1704) | `battery_firmware_version` | uint16 | |
| IR(1705) | `e_aio1_charge_today` | deci | kWh today |
| IR(1706-1707) | `e_aio1_charge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1708) | `e_aio2_charge_today` | deci | kWh today |
| IR(1709-1710) | `e_aio2_charge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1711) | `e_aio3_charge_today` | deci | kWh today |
| IR(1712-1713) | `e_aio3_charge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1750) | `e_aio1_discharge_today` | deci | kWh today |
| IR(1751-1752) | `e_aio1_discharge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1753) | `e_aio2_discharge_today` | deci | kWh today |
| IR(1754-1755) | `e_aio2_discharge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1756) | `e_aio3_discharge_today` | deci | kWh today |
| IR(1757-1758) | `e_aio3_discharge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |

**IR 1795-1818 — Battery/AIO SOC and power:**

| Registers | Field | Converter | Notes |
|---|---|---|---|
| IR(1795) | `e_battery_charge_today` | deci | kWh today |
| IR(1796-1797) | `e_battery_charge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1798) | `e_battery_discharge_today` | deci | kWh today |
| IR(1799-1800) | `e_battery_discharge_total` | uint32 / deci | V1: hi,lo; V2: lo,hi |
| IR(1801) | `aio1_soc` | uint16 | 0-100% |
| IR(1802) | `aio2_soc` | uint16 | 0-100% |
| IR(1803) | `aio3_soc` | uint16 | 0-100% |
| IR(1816) | `p_aio1_inverter` | int16 | AIO 1 inverter power (W) |
| IR(1817) | `p_aio2_inverter` | int16 | AIO 2 inverter power (W) |
| IR(1818) | `p_aio3_inverter` | int16 | AIO 3 inverter power (W) |

**IR 1831-1859 — AIO serial numbers (addresses differ by firmware variant):**

| Variant | AIO 1 | AIO 2 | AIO 3 |
|---|---|---|---|
| V1 (≤ GA000009) | IR(1831-1835) | IR(1838-1842) | IR(1845-1849) |
| V2 (≥ GA000010) | IR(1841-1845) | IR(1848-1852) | IR(1855-1859) |

##### Holding registers — configuration (per GivTCP)

GivTCP's `core_regs` for Gateway (family `"7"`):

```
HR 0-59   (identity — covered by standard poll)
HR 60-119 (config part 1 — covered by standard poll)
HR 120-179 (config part 2)
HR 180-239 (config part 3)
HR 240-299 (extended slots — likely unused on Gateway)
HR 300-359 (AC config — export priority, limits, EPS, pause)
```

GivTCP's `add_regs` for Gateway adds HR 180, 240, 300 for the additional
configuration blocks.

#### Firmware variants (V1 vs V2)

The Gateway has two firmware variants with different uint32 byte ordering
for energy totals and different serial number register addresses.

| Aspect | V1 (≤ GA000009) | V2 (≥ GA000010) |
|---|---|---|
| Detection | `IR(1603) < 10` | `IR(1603) >= 10` |
| uint32 energy totals | hi,lo register order | lo,hi (swapped) |
| AIO 1 serial | IR(1831-1835) | IR(1841-1845) |
| AIO 2 serial | IR(1838-1842) | IR(1848-1852) |
| AIO 3 serial | IR(1845-1849) | IR(1855-1859) |

Detection logic from `givenergy-modbus` `select_gateway()`: read `IR(1603)`
(last register of the version string); raw value < 10 → V1, >= 10 → V2.

#### Gateway fault code decoding

The 32-bit fault bitmask at IR(1622-1623) uses MSB-first bit numbering:

| Bit | Fault |
|---|---|
| 0 | Relay 1&2 bonding |
| 1 | Relay 3&4 bonding |
| 2 | Relay 1&2 disconnect |
| 3 | Relay 3&4 disconnect |
| 4 | AC over frequency 1 |
| 5 | AC under frequency 1 |
| 6 | AC over voltage 1 |
| 7 | AC under voltage 1 |
| 8-11 | AC over/under frequency/voltage 2 |
| 13 | No zero-point protection |
| 14 | Over quarter AC voltage |
| 15 | Under quarter AC voltage |
| 16 | Over AC voltage long-time |
| 17-20 | AC over/under frequency/voltage constant |
| 31 | Grid mode Off |

#### GivTCP Gateway handling reference

GivTCP treats the Gateway identically to three-phase for charge/discharge
rate writes:

```python
# write.py — charge rate scaling
if "3ph" in inverter_type or "gateway" in inverter_type:
    target = round((charge_rate / invmaxrate) * 100, 0)  # 1-100%
    reqs = commands.set_battery_charge_limit_ac(target)
else:
    target = int(min((charge_rate / (batcap/2)) * 50, 50))  # 0-50%
```

Battery capacity and max rate are derived from the number of parallel AIOs:

```python
# read.py — getInvModel()
if model == Model.GATEWAY:
    batmaxrate = 6000 * int(parallel_aio_num)
    batterycapacity = 13.5 * int(parallel_aio_num)
```

Gateway uses the same AC-coupled write targets (HR 313/314 for limits,
HR 1108/1109/1110/1111 for three-phase-style config) — it inherits the
three-phase command set because it coordinates AIOs that are themselves
AC-coupled battery units.

#### Mapping Gateway fields to InverterSnapshot

The Gateway's IR 1600+ fields can be mapped to the existing `InverterSnapshot`
structure:

| InverterSnapshot field | Gateway source | Notes |
|---|---|---|
| `solar_power` | IR(1617) `p_pv` | uint16 W |
| `battery_power` | IR(1619) `p_liberty` or sum of `p_aio*_inverter` | Signed W |
| `grid_power` | IR(1616) `p_ac1` | int16 W |
| `home_power` | IR(1618) `p_load` | uint16 W |
| `grid_voltage` | IR(1608) `v_grid` / 10 | |
| `grid_frequency` | Not directly in Gateway register map | May need different source |
| `soc` | IR(1801) `aio1_soc` (or average of 1801-1803) | 0-100% |
| `battery_capacity_kwh` | `13.5 × parallel_aio_num` | From GivTCP |
| `max_battery_power_w` | `6000 × parallel_aio_num` | From GivTCP |
| `today_solar_kwh` | IR(1643) `e_pv_today` / 10 | |
| `today_import_kwh` | IR(1640) `e_grid_import_today` / 10 | |
| `today_export_kwh` | IR(1646) `e_grid_export_today` / 10 | |
| `today_charge_kwh` | IR(1649) `e_aio_charge_today` / 10 | |
| `today_discharge_kwh` | IR(1652) `e_aio_discharge_today` / 10 | |
| `today_consumption_kwh` | IR(1655) `e_load_today` / 10 | |
| `total_import_kwh` | IR(1641-1642) `e_grid_import_total` / 10 | V1/V2 byte order |
| `total_export_kwh` | IR(1647-1648) `e_grid_export_total` / 10 | V1/V2 byte order |
| `device_type` | HR(0) → `DeviceType::Gateway` | |
| `firmware_version` | IR(1600-1603) `software_version` | e.g. "GA000010" |
| `battery_state` | Derived from `p_aio_total` sign | |

New Gateway-specific fields to add to `InverterSnapshot`:

| Field | Type | Source | Notes |
|---|---|---|---|
| `parallel_aio_count` | u8 | IR(1700) | 1-3 |
| `parallel_aio_online` | u8 | IR(1701) | |
| `per_aio_soc` | `[u8; 3]` | IR(1801-1803) | Per-unit SOC |
| `per_aio_power` | `[i32; 3]` | IR(1816-1818) | Per-unit inverter power |
| `per_aio_charge_today_kwh` | `[f32; 3]` | IR(1705, 1708, 1711) | |
| `per_aio_discharge_today_kwh` | `[f32; 3]` | IR(1750, 1753, 1756) | |
| `gateway_fault_codes` | Vec<String> | IR(1622-1623) | Decoded fault names |
| `gateway_software_version` | String | IR(1600-1603) | e.g. "GA000010" |
| `gateway_work_mode` | String | IR(1604) | Work mode enum name |
| `gateway_is_v2` | bool | IR(1603) >= 10 | Firmware variant flag |

#### Implementation plan

##### Phase 1: Read-only monitoring (polling + decoding)

**1.1 New register blocks** — `src-tauri/src/modbus/registers.rs`:

Add Gateway-specific IR blocks (per `givenergy-modbus` `refresh()`):

```rust
/// Gateway system state: software version, work mode, grid/load/PV measurements.
pub const GATEWAY_INPUT_BLOCK_1: RegisterBlock = RegisterBlock {
    start: 1600, count: 60, register_type: Input, name: "input_1600_1659",
};
/// Gateway per-AIO energy and battery data.
pub const GATEWAY_INPUT_BLOCK_2: RegisterBlock = RegisterBlock {
    start: 1660, count: 60, register_type: Input, name: "input_1660_1719",
};
pub const GATEWAY_INPUT_BLOCK_3: RegisterBlock = RegisterBlock {
    start: 1720, count: 60, register_type: Input, name: "input_1720_1779",
};
pub const GATEWAY_INPUT_BLOCK_4: RegisterBlock = RegisterBlock {
    start: 1780, count: 60, register_type: Input, name: "input_1780_1839",
};
pub const GATEWAY_INPUT_BLOCK_5: RegisterBlock = RegisterBlock {
    start: 1840, count: 20, register_type: Input, name: "input_1840_1859",
};
pub const GATEWAY_INPUT_BLOCKS: &[RegisterBlock] = &[
    GATEWAY_INPUT_BLOCK_1, GATEWAY_INPUT_BLOCK_2,
    GATEWAY_INPUT_BLOCK_3, GATEWAY_INPUT_BLOCK_4,
    GATEWAY_INPUT_BLOCK_5,
];
```

**1.2 Gateway HR configuration blocks** — `src-tauri/src/modbus/registers.rs`:

In addition to the IR input blocks above, define HR config blocks for Gateway
(per GivTCP `core_regs` and `add_regs`):

```rust
/// Gateway HR config: HR 120-179 (config part 2).
pub const GATEWAY_HIGH_HR_BLOCK_1: RegisterBlock = RegisterBlock {
    start: 120, count: 60, register_type: Holding, name: "gateway_hr_120_179",
};
/// Gateway HR config: HR 180-239 (config part 3).
pub const GATEWAY_HIGH_HR_BLOCK_2: RegisterBlock = RegisterBlock {
    start: 180, count: 60, register_type: Holding, name: "gateway_hr_180_239",
};
/// Gateway HR config: HR 240-299 (extended slots — likely unused).
pub const GATEWAY_HIGH_HR_BLOCK_3: RegisterBlock = RegisterBlock {
    start: 240, count: 60, register_type: Holding, name: "gateway_hr_240_299",
};
/// Gateway HR config: HR 300-359 (AC config — export priority, limits, EPS).
pub const GATEWAY_HIGH_HR_BLOCK_4: RegisterBlock = RegisterBlock {
    start: 300, count: 60, register_type: Holding, name: "gateway_hr_300_359",
};
pub const GATEWAY_HR_BLOCKS: &[RegisterBlock] = &[
    GATEWAY_HIGH_HR_BLOCK_1, GATEWAY_HIGH_HR_BLOCK_2,
    GATEWAY_HIGH_HR_BLOCK_3, GATEWAY_HIGH_HR_BLOCK_4,
];
```

**1.3 DeviceType trait updates** — `src-tauri/src/inverter/model.rs`:

```rust
// Add to DeviceType impl:
pub fn needs_gateway_input_blocks(&self) -> bool {
    matches!(self, Self::Gateway)
}

// Update extra_poll_blocks() to include Gateway HR config blocks.
// Note: only HR blocks go here; IR input blocks use a separate path
// via model_specific_blocks_in_poll_order() in client.rs.
Self::Gateway => &GATEWAY_HR_BLOCKS,

// Update max values — Gateway max depends on aio_count (dynamic),
// so this can only be a best-effort fallback until the decoder sets it:
Self::Gateway => 12000,  // keep hardcoded for now
```

Also update `needs_three_phase_input_blocks()` — Gateway does NOT use
the same IR 1000+ range, so this must remain false:

```rust
pub fn needs_three_phase_input_blocks(&self) -> bool {
    matches!(
        self,
        Self::ThreePhase | Self::ACThreePhase | Self::AioCommercial
            | Self::HybridHvGen3 | Self::AllInOneHybrid
    )
    // Gateway is NOT included here — it uses IR 1600+ instead.
}
```

**1.4 Model-specific poll blocks** — `src-tauri/src/modbus/client.rs`:

The three-phase input blocks (IR 1000-1413) are added to the poll list
via `model_specific_blocks_in_poll_order()`. Gateway IR 1600+ blocks must
be added the same way — NOT via `extra_poll_blocks()` which only handles
HR config blocks:

```rust
fn model_specific_blocks_in_poll_order(
    device_type: &crate::inverter::model::DeviceType,
) -> Vec<&'static RegisterBlock> {
    let mut blocks = Vec::new();

    if device_type.needs_three_phase_input_blocks() {
        blocks.extend(super::registers::THREE_PHASE_INPUT_BLOCKS.iter());
    }

    // 👇 NEW: Gateway IR 1600+ telemetry blocks
    if device_type.needs_gateway_input_blocks() {
        blocks.extend(super::registers::GATEWAY_INPUT_BLOCKS.iter());
    }

    blocks.extend(device_type.extra_poll_blocks().iter());
    blocks
}
```

Also in `read_all_with_extras()`, the `STANDARD_POLL_BLOCKS` selection
logic must be updated. Gateway still needs HR 0-59 (identity/serial) but
IR 0-59 and IR 180-239 are likely irrelevant (same as EMS). Consider
either:

- Keeping the full `STANDARD_POLL_BLOCKS` (safe but wasteful — Gateway
  IR 0-59 / IR 180-239 will likely time out)
- Creating a `STANDARD_POLL_BLOCKS_GATEWAY` that includes only HR blocks
  - HR identity

**1.5 Gateway decoder** — `src-tauri/src/inverter/decoder.rs`:

Add `decode_gateway_state()` function that takes IR 1600-1859 register
data and populates `InverterSnapshot` fields:

- Map power measurements to snapshot fields
- Decode energy totals with V1/V2-aware uint32 byte ordering
- Extract per-AIO SOC, power, and energy
- Decode 32-bit fault bitmask into human-readable strings
- Decode `gateway_version` string (4-register Latin-1, same as HV BCU
  `decode_gateway_version()` which already exists in decoder.rs)
- Compute aggregate SOC (average of online AIOs)
- Compute `battery_capacity_kwh` = `13.5 × parallel_aio_num`
- Compute `max_battery_power_w` = `6000 × parallel_aio_num`
- Handle V1 vs V2 byte ordering for uint32 energy totals
- Leave `grid_frequency` as NAN (not available in Gateway register map)

**1.6 Poll loop integration** — `src-tauri/src/inverter/poll.rs`:

The IR 1600+ blocks are already handled by `model_specific_blocks_in_poll_order()`
in client.rs (step 1.4 above) — no changes needed in poll.rs for the polling
itself. However, poll.rs needs:

- Gateway data routing: when `needs_gateway_input_blocks()` is true,
  route IR 1600+ data blocks to the new `decode_gateway_state()`
- Carry-forward for optional Gateway HR blocks (same pattern as
  three-phase optional block carry-forward)
- Gateway-specific sanitization ranges: grid voltage 0-500V, load
  power up to 12kW+, PV power up to 12kW+, SOC 0-100%
- The `derive_battery_fields_from_bms()` function needs updating:
  Gateway has no HV BCU cluster — battery fields come from the decoder's
  `parallel_aio_num`-based computation, not from BMS
- Meter probing: Gateway uses its own grid CT (IR 1609), not external
  meters — set `is_three_phase`-style skip when `needs_gateway_input_blocks()`

**1.7 Snapshot model extensions** — `src-tauri/src/inverter/model.rs`:

Add Gateway-specific fields to `InverterSnapshot` (with `#[serde(default)]`
for backward compatibility):

- `parallel_aio_count`, `parallel_aio_online`
- `per_aio_soc`, `per_aio_power`
- `per_aio_charge_today_kwh`, `per_aio_discharge_today_kwh`
- `gateway_fault_codes`, `gateway_software_version`, `gateway_work_mode`
- `gateway_is_v2`

##### Phase 2: Configuration and control (writes)

**2.1 Gateway encoder support** — `src-tauri/src/inverter/encoder.rs`:

Gateway uses the same AC-coupled / three-phase write targets as AC3:

| Control | Register | Range |
|---|---|---|
| Charge rate | HR 1110 (or HR 313) | 1-100% |
| Discharge rate | HR 1108 (or HR 314) | 1-100% |
| SOC reserve | HR 1109 | 0-100% |
| Charge target SOC | HR 1111 | 0-100% |
| Export priority | HR 311 | 0/1/2 |
| EPS enable | HR 317 | bool |

Per GivTCP, Gateway is treated identically to 3ph for rate scaling
(charge/discharge watt → 1-100% AC limit register).

**2.2 API routes** — `src-tauri/src/server/api.rs`:

Remove `Gateway` from the `supports_schedule_slots()` exclusion set in
relevant API routes if Gateway-specific schedule registers are discovered.
Initially, schedule control is likely delegated to the individual AIOs,
not the Gateway itself.

##### Phase 3: Frontend display

**3.1 StatusPage** — Show per-AIO data when Gateway is detected:

- Aggregate power flow in EnergyFlowDiagram
- Per-AIO SOC, power, and energy in a summary panel
- Gateway firmware version and work mode in device info

**3.2 InverterPage** — Show Gateway-specific info:

- Software version ("GA000010" etc.)
- Parallel AIO count and online count
- Per-AIO serial numbers
- Gateway work mode
- Battery firmware version

**3.3 HistoryPage** — Energy totals for charting:

- Aggregate AIO charge/discharge energy
- Per-AIO breakdown (optional)

#### Testing strategy

- **Unit tests**: Gateway decoder tests with synthetic register data
- **Firmware variant tests**: V1 vs V2 byte ordering for uint32 energy totals
- **Fault code tests**: Known bitmask → expected fault name list
- **Integration tests**: Mock TCP server that simulates Gateway dongle
  responses for IR 1600+ registers
- **GivEnergy Simulator**: Add Gateway device type to the simulator if
  possible, to exercise the real protocol stack

#### Open questions

1. **Battery temperature**: The Gateway register map doesn't appear to
   expose per-AIO battery temperature. Need to confirm if this is available
   elsewhere or if temperature reporting is simply unavailable for Gateway.

2. **PV voltage/current**: IR(1612) has `i_pv` but there's no `v_pv` or
   `p_pv1`/`p_pv2` split. The Gateway may aggregate PV data from the AIOs
   rather than having its own PV inputs.

3. **Grid frequency**: Not visible in the Gateway register map. May need to
   be omitted from the display or sourced from a different register.

4. **Battery mode derivation**: The standard `BatteryMode::from_registers()`
   uses HR(27), HR(59), HR(110) which are single-phase registers. Gateway
   uses HR(300-359) for AC config — need to confirm whether HR(27)/HR(59)
   are populated on the Gateway or if work_mode (IR 1604) replaces them.

5. **Single AIO attached**: GivTCP warns that a Gateway with a single AIO
   provides mostly duplicate data. Consider showing a similar hint in the
   UI.

6. **Discovery**: The standard Modbus discovery in `discovery.rs` sends a
   read request and validates the 0x5959 magic header. Need to confirm
   Gateway dongles respond to the same discovery protocol.

---

### GitHub Actions Node runtime update

GitHub Actions currently reports a non-fatal Node 20 deprecation warning for
some marketplace actions. Update affected actions or opt in to Node 24 when the
actions used by the workflow support it cleanly.

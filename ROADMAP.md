# Roadmap

Planned and under-investigation items for Home Energy Manager. This is not a
release commitment; items may change as hardware access, simulator support, and
user reports improve.

## Solar clipping & PV string-loss alerts

**Status**: Design proposed; not implemented. Requested by the alert user who
wanted two new alert types alongside the existing temperature/SOC/grid ones.

Two distinct alerts, both of which are **financial/performance** rather than
safety alerts. That flips the cost asymmetry from the over-temp alert: a missed
clipping event costs a few pence in lost generation, but a false-positive storm
erodes trust in every other alert. So both designs bias hard toward precision
— accept false-negatives, never false-positives.

### 1. Solar clipping alert

Clipping = the inverter's AC output is capped at its rated limit on a bright
day. **"Output near the limit" is the wrong trigger** — sitting at rated power
on a clear day is correct operation and would fire constantly. The real
signature is a **sustained flat-top at the ceiling**: output pinned within
~2–3% of the limit with near-zero variance over several minutes. The *flatness*
(plateau) is the tell that the MPPT has been curtailed — a non-clipping bright
day still has cloud/gust ripple.

**Trigger (all must hold):**

1. Inverter AC output within ~2% of the effective limit, **and**
2. Rolling std-dev over ~5 min is very low (flat-top), **and**
3. Sustained for ≥ N cycles (reuse the consecutive-read confirmation pattern
   from `confirm_battery_warning()`), **and**
4. Daylight — trivially satisfied since output near the ceiling can't happen
   at night.

**The "limit" source — three tiers:**

- **Own inverter output:** use `snap.max_ac_power_w`, derived from the device-
   type code (DTC) via `DeviceType::max_ac_power_w()` /
   `max_ac_power_for_dtc()` (5000W for Gen2/Gen3/Polar hybrids, 3600W for the
   3.6kW AIO, 6000W for 6kW/three-phase, etc.). No config needed.
- **Manual override:** user enters a derated limit (e.g. "I cap at 4500W" or
   "my DTC table is wrong"). Overrides the DTC value when set.
- **External CT (e.g. a separate PV inverter on a clamp):** no nameplate is
   available from the inverter data, so **manual limit is the only reliable
   option**. A "learned ceiling" (rolling max over N weeks) is fragile to
   seasons and self-defeating if it always clips — skip auto-learn; require
   manual for CT-sourced PV.

**Open question to resolve before implementing:** the snapshot exposes
`solar_power` (= DC, `pv1_power + pv2_power`), but clipping is an AC phenomenon.
We may need to derive inverter AC output as `solar_power + battery_discharge
− battery_charge` (power onto the AC bus), or confirm there is a direct live
AC-output register. Worth a focused dig into `decoder.rs` before building so we
compare the right quantity to `max_ac_power_w`.

### 2. PV string / circuit loss alert

Genuinely the harder one because of three confounders: **night**,
**string-2-never-installed**, and **transients**. Each needs a specific
defence, and together they make it reliable:

**A. Sibling-string daylight proxy (solves night).** Don't detect "is it
daytime" in absolute terms. Instead: **PV2 is only declared lost if PV1 is
clearly producing** (`pv1_power > ~150–200W`, bright enough that PV2 should
also be making something) **and** `pv2_power ≈ 0`. Under the same sun, two
strings on the same roof can't diverge that much unless one is broken. PV1 is
the irradiance reference — at night both read ~0, so the condition can't fire.
(Acceptable trade-off: if both strings ever read 0, no alert — total darkness
isn't a fault.)

**B. "Ever produced" auto-detection (solves not-installed).** Keep a
per-string "has ever produced" flag learned over the first day or two of
running. **PV2-loss alerts only ever fire if PV2 has been seen producing
meaningful power at least once.** If PV2 is simply never attached, the flag
never sets → zero false alarms — no manual "PV2 installed: yes/no" needed in
the common case. A real failure is then a genuine *change*: it used to
produce, now it's gone.

**C. Consecutive-read confirmation (solves transients).** Same pattern as the
over-temp fix: require N consecutive cycles of "PV1 high + PV2 dead" before
firing. One corrupted 0-read on IR(20) can't trigger it.

**D. Optional manual override** for edge cases: a "strings installed" config
(`Auto` / `One` / `Two`) for installs where auto-detection is ambiguous.
Default = `Auto`.

**Trigger (all must hold):**

1. `pv1_power` above the daylight threshold for ≥ N cycles (it's bright),
   **and**
2. `pv2_power ≈ 0` for the same ≥ N cycles, **and**
3. PV2's "ever produced" flag is set (so we know it's installed), **and**
4. Not in the system's startup/learning window.

**Optional asymmetry-ratio variant** (later, not first cut): instead of only
"pv2 dead", alert when `pv1/pv2` (or vice-versa) exceeds an expected ratio for
a sustained period — catches a half-shaded or degraded string, not just a dead
one. More false-positive-prone; ship dead-string first.

### Shared building blocks

- **Consecutive-read confirmation** — `confirm_battery_warning()` on
  `AlertDebounce` exists; generalise it into a small "streak" helper so both
  new alerts reuse the same proven mechanism.
- **Per-string "ever produced" / rolling-stats state** belongs on
  `AlertDebounce` (or a sibling struct) exactly like
  `battery_warning_streak` does today — device-lifecycle state, reset on
  `clear()`.
- **Daylight helper** — a single function "is it daytime" based on the
  sibling-string proxy, shared by both alerts (clipping doesn't strictly need
  it but it's a cheap belt-and-braces gate).

### Proposed config shape

Extending `AlertsConfig` (`settings/mod.rs`):

```
solar_clipping_enabled: bool
solar_clipping_limit_w: Option<u32>   // manual override; None = use DTC max_ac_power_w
solar_clipping_minutes: u32           // sustained window (default ~5)

pv_string_loss_enabled: bool
pv_strings_installed: enum { Auto, One, Two }   // default Auto (ever-produced)
pv_string_loss_minutes: u32                      // sustained window
```

Manual limits appear exactly where needed (clipping limit override + the
strings-installed override), but are **optional** — defaults use DTC +
auto-detection so a typical user configures nothing.

### Open questions to confirm before building

1. **Clipping quantity** — is there a live AC-output register, or is deriving
   it from `solar_power ± battery_power` acceptable?
2. **Strings installed** — happy with "Auto via ever-produced" as the default,
   manual override only?
3. **Dead-string only** vs dead + asymmetry (degraded string) — ship dead
   first?
4. **External-CT clipping** — manual limit only, agreed (no learned ceiling)?

---

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

1. **Report metadata and summary **
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

**Status**: Investigation complete; implementation plan written, not yet started.

The GivEnergy Gateway is a system controller / AC hub for up to 3 All-in-One
(AIO) battery units — it is **not an inverter**. It exposes a unique Input
Register bank (IR 1600–1859) that aggregates telemetry from its child AIO(s) and
reports system-wide grid/PV/load (measured by its own built-in meter, excluding
the EV charger) plus per-AIO battery SOC/power/energy. It has **zero
directly-attached batteries** and exposes **no per-cell telemetry** (cells live
on each AIO's own BMS, reachable only via a separate direct connection).

**Authoritative design docs** (topology, register map, byte-level reference)
live in [`gateway-design/`](./gateway-design/):

- [`gateway-integration-guide.md`](./gateway-design/gateway-integration-guide.md) — connection model + display data model
- [`gateway-register-reference.md`](./gateway-design/gateway-register-reference.md) — byte-level register map, converters, V1/V2 variant contract
- [`gateway-client-integration.md`](./gateway-design/gateway-client-integration.md) — display/UI overview

The phased build plan is [`gateway-design/IMPLEMENTATION-PLAN.md`](./gateway-design/IMPLEMENTATION-PLAN.md).

**Current HEM support** (stub only): `DeviceType::Gateway` is detected
(`from_register()` maps `0x70xx` → `Gateway`; `display_name()` → `"Gateway"`;
`preferred_read_slave_address()` → `0x11`), but there is no gateway polling, no
IR 1600+ decoder, no gateway-specific snapshot fields, no write support, and no
UI. `max_battery_power_w()` returns 0 (the true value is dynamic: `6000 ×
parallel_aio_num`, set by the decoder once the gateway bank is read).

---

### GitHub Actions Node runtime update

GitHub Actions currently reports a non-fatal Node 20 deprecation warning for
some marketplace actions. Update affected actions or opt in to Node 24 when the
actions used by the workflow support it cleanly.


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

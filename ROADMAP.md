# Roadmap

Planned and under-investigation items for Home Energy Manager. This is not a
release commitment; items may change as hardware access, simulator support, and
user reports improve.

## Mobile Experience (iOS/iPhone)
These items address accessibility and visual polish when the app is saved to the home screen (PWA mode), as reported in [Issue #63](https://github.com/psylsph/home-energy-manager/issues/63).

- **App Icon**: Implement custom apple-touch-icons to replace the default placeholder.
- **Safe Area Insets**: Use `env(safe-area-inset-bottom)` to raise the bottom navigation toolbar, preventing it from being cut off by rounded screen edges on iPhones.
- **Energy Flow Enhancements**:
  - Increase size of flow nodes (bubbles) and connecting line thickness.
  - Implement dynamic stroke width for flow lines based on energy volume (e.g., thicker lines for higher power).

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

### Multi-Zone Tariff Cost Calculations

**Status**: Planning complete. Related to [Issue #64](https://github.com/psylsph/home-energy-manager/issues/64).

The current 2-zone (Peak/Off-Peak) model cannot represent tariffs like Octopus Flux (3 zones) or Cosy (4+ zones). This project will implement a generic N-zone tariff system to accurately compute costs on the History page.

#### Technical Design
- **Data Model**: Introduce `TariffZone` (label, rate, start, end) and `MultiZoneTariffConfig` (list of zones + default day rate).
- **Tariff Mode**: A new `tariff_mode` setting (`standard` vs `multizone`) allows seamless fallback to existing 2-zone configs.
- **Rate Resolution**: A generic `getRate(timestamp)` function replaces the binary `isOffPeak()` logic. It iterates through zones to find a match, falling back to the `default_rate`.
- **UI**: Preset buttons for **Flux** and **Cosy** that pre-fill complex zone configurations, with a "Custom" mode for arbitrary zone creation.

#### Implementation Order
1. **Backend**: Update `Settings` struct $\rightarrow$ `Serde` serialization tests $\rightarrow$ `GET/PUT /api/settings` endpoints.
2. **Frontend State**: Update `types.ts` $\rightarrow$ wire `PollSettings` to `SettingsPage.tsx` and `HistoryPage.tsx`.
3. **UI Construction**: Build the zone editor in `SettingsPage.tsx` with preset-switching logic.
4. **Engine Update**: Implement `getMultiZoneRate()` and update the cost `preprocess` pipeline in `HistoryPage.tsx`.
5. **Visuals**: Add per-zone cost breakdown summary and optional stacked area charts.

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

### GitHub Actions Node runtime update

GitHub Actions currently reports a non-fatal Node 20 deprecation warning for
some marketplace actions. Update affected actions or opt in to Node 24 when the
actions used by the workflow support it cleanly.

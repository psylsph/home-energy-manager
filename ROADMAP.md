# Roadmap

Planned and under-investigation items for Home Energy Manager. This is not a
release commitment; items may change as hardware access, simulator support, and
user reports improve.

## Near-term candidates

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

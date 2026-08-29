# Design & Architecture

Technical reference for GivEnergy Local. For a user-oriented overview, see [README.md](./README.md).

## System Architecture

```
┌───────────────────────────────────────────────────────────────┐
│                        Tauri Desktop App                      │
│                                                               │
│  ┌──────────────────────┐     ┌─────────────────────────────┐ │
│  │    React Frontend     │     │       Rust Backend          │ │
│  │                      │     │                             │ │
│  │  StatusPage          │     │  Axum HTTP Server :7337     │ │
│  │  BatteryPage         │◄───►│    ├─ /api/* (REST)         │ │
│  │  HistoryPage         │ WS  │    └─ /ws    (WebSocket)    │ │
│  │  ControlPage         │     │                             │ │
│  │  SettingsPage        │     │  Poll Loop ─────────┐       │ │
│  │                      │     │    read registers    │       │ │
│  │  Zustand store       │     │    write registers   │       │ │
│  │  useWebSocket hook   │     │    broadcast updates │       │ │
│  └──────────────────────┘     └──────────┬──────────┘       │ │
│                                          │                   │ │
│                                    Modbus TCP :8899          │ │
└──────────────────────────────────────────┼───────────────────┘
                                           │
                                  ┌────────▼─────────┐
                                  │  Data Adapter     │
                                  │  (dongle)         │
                                  └────────┬──────────┘
                                           │ serial
                                  ┌────────▼─────────┐
                                  │  Inverter + BMS   │
                                  └──────────────────┘
```

## Frontend

**Stack**: React 19, TypeScript, Vite 8, Tailwind CSS 4, Zustand, Recharts, React Router 7

### Key files

| File | Purpose |
|---|---|
| `src/main.tsx` | App entry, router, Zustand store provider |
| `src/lib/api.ts` | `apiGet`/`apiPost` fetch helpers (both check `res.ok`) |
| `src/lib/types.ts` | `InverterSnapshot` interface (mirrors Rust struct) |
| `src/lib/format.ts` | Power (W), voltage (V), current (A), temp (°C), percent formatters |
| `src/hooks/useWebSocket.ts` | Connects to `/ws`, auto-reconnects, fetches initial REST snapshot |
| `src/components/EnergyOrbitDiagram.tsx` | Radial SVG with animated power flow lines (renamed from `EnergyFlowDiagram`) |
| `src/components/BatteryPanel.tsx` | Per-battery-module cell voltage/temperature table |
| `src/pages/ForecastPage.tsx` | 48 h solar / consumption / battery-projection charts + overnight charge plan (issue #283) |
| `src/pages/OctopusPage.tsx` | Octopus smart-meter dashboard — supplier import/export/gas, billing, CSV/PDF export (issue #212) |
| `src/pages/ControlPage.tsx` | Schedule slots, mode selector, SOC/limit sliders |
| `src/pages/SettingsPage.tsx` | Connection config, discovery, tariffs, alerts, about section |

### State management

Zustand store (`useInverterStore`):

```typescript
{
  snapshot: InverterSnapshot | null,
  connectionState: ConnectionState,
  connectedHost: string | null,
  developerMode: boolean,        // persisted to localStorage
  themeMode: ThemeMode,
  readOnly: boolean,
  hiddenPanels: string[],
  chartRange: HistoryRange,
  gridLineWeight: GridLineWeight,
  // EV Charger state (evcHost, evcPower, evcCharging, evcConnected,
  // evcEverConnected latch, session energy …) — the latch distinguishes
  // "charger was here, now offline" from "never reached" (issue #138)
}
```

Updated via WebSocket messages. All pages read from this single store.

### Version display

App version is injected at build time via `vite.config.ts` → `__APP_VERSION__` global constant, declared in `src/env.d.ts`. Displayed on Settings → About.

## Backend

**Stack**: Rust, Tauri 2, Axum, Tokio, Chrono, CRC

### Module structure

```
src-tauri/src/
├── lib.rs              Tauri setup + headless CLI, spawns server + poll task
├── main.rs             Tauri builder entry point
├── update.rs           "New version available" GitHub Releases polling
├── octopus.rs          Octopus Energy account integration (issue #212)
├── windows_autostart.rs Windows registry autostart helper
├── test_util.rs        Shared test fixtures
├── inverter/
│   ├── mod.rs          Re-exports
│   ├── model.rs        InverterSnapshot, ScheduleSlot, BatteryMode, DeviceType
│   ├── decoder.rs      Register → snapshot decoder, timeslot logic, enable flag gating
│   ├── encoder.rs      ControlCommand → RegisterWrite encoder, whitelist validation
│   ├── poll.rs         Poll loop: write queue → register reads → snapshot broadcast
│   ├── sanitizer.rs    Register-corruption defense (range/delta/median checks)
│   ├── state_machines.rs  Connect/reconnect + battery-protocol + Cosy/Agile automations
│   ├── reconnect.rs    Reconnect backoff
│   └── discovery.rs    Network scan, subnet inference, serial auto-detect
├── modbus/
│   ├── mod.rs          Re-exports
│   ├── client.rs       ModbusClient: connect, read, write (FC6), stale frame drain
│   ├── framer.rs       GivEnergy frame encode/decode (proprietary MBAP variant)
│   └── registers.rs    Register addresses, poll blocks, safe-write list, HHMM codec
├── history/            SQLite history + forecast persistence (~/.givenergy-local/history.db)
├── forecast/           Solar forecast, consumption profile, SOC simulation, planner (#283)
├── weather/            Open-Meteo ambient-temperature fetch + backfill
├── alerts/             Threshold alerts, Telegram/ntfy/Pushover delivery, daily report
├── evc/                EV charger (OCPP/Modbus) client + poll loop
├── server/
│   ├── mod.rs          Axum router, server startup (graceful error handling)
│   ├── api.rs          REST endpoints (/api/control/*, /api/snapshot, /api/settings …)
│   ├── ws.rs           WebSocket handler, PollMessage broadcast
│   └── logs.rs         LogRing + GET /api/logs
└── settings/
    └── mod.rs          JSON file persistence (~/.givenergy-local/settings.json)
```

### Poll loop lifecycle

```
┌─────────┐    ┌──────────┐    ┌──────────────┐    ┌───────────┐
│ Connect ├───►│ Poll loop ├───►│ Read regs    │───►│ Broadcast │
│         │    │ (inner)   │    │ Decode snap  │    │ via WS    │
└────▲────┘    └─────┬─────┘    └──────────────┘    └───────────┘
     │               │
     │         ┌─────▼──────┐
     │         │ Sleep      │
     │         │ (wake on:  │
     │         │  interval, │
     │         │  write     │
     │         │  notify,   │
     │         │  settings  │
     │         │  change)   │
     │         └────────────┘
     │               
   Reconnect on 3 consecutive read failures or settings change
```

Key: when the API queues writes, `write_notify.notify_one()` wakes the sleep immediately. Writes are drained before reads on each cycle.

After decoding, **external solar CT meters** are merged into the snapshot. Each configured `SolarArrayConfig` (settings `solar_arrays`) reads its CT clamp by device address (1–8; 0x00 is the synthetic grid CT). For AC-coupled setups the solar CT clamp is the **authoritative solar reading** — the inverter's PV registers aren't used for solar, so Overview, the Status wheel, and Solar Arrays all report the same figure by construction (issue #277). Midnight baselines track cumulative CT energy counters per meter address.

### Adaptive Charge state machine

Adaptive Charge runs a state machine (`src-tauri/src/inverter/state_machines.rs`) driven from the poll loop. A **minimum SOC guard** on discharge schedules holds the battery at the configured floor: when a discharge slot would take SOC below the floor, discharging pauses and resumes once SOC recovers.

### Shared state (AppState)

```rust
pub struct AppState {
    pub latest_snapshot: Arc<Mutex<Option<InverterSnapshot>>>,
    pub connection_state: Arc<Mutex<ConnectionState>>,
    pub tx: broadcast::Sender<PollMessage>,
    pub settings: Arc<Mutex<PollSettings>>,
    pub pending_writes: Arc<Mutex<Vec<Vec<RegisterWrite>>>>,
    pub write_notify: Arc<Notify>,
    pub history: Arc<Mutex<Option<Arc<HistoryDb>>>>,
    pub log_ring: Arc<LogRing>,
    pub connected_clients: Arc<parking_lot::Mutex<ConnectedClients>>,
    pub auto_winter_config: Arc<Mutex<AutoWinterConfig>>,
    pub auto_winter_state: Arc<Mutex<AutoWinterState>>,
    pub auto_winter_saved: Arc<Mutex<Option<AutoWinterSaved>>>,
}
```

## History Database

SQLite-backed time-series storage at `~/.givenergy-local/history.db`. One row per poll cycle.

### Schema (`readings` table)

29 columns — timestamp (epoch seconds, PK) + all telemetry fields. Key energy columns:

| Column | Type | Source Register | Description |
|---|---|---|---|
| `today_solar_kwh` | REAL | IR 17+19 (×0.1) | PV energy today (kWh) |
| `today_import_kwh` | REAL | IR 26 (×0.1) | Grid import today (kWh) |
| `today_export_kwh` | REAL | IR 25 (×0.1) | Grid export today (kWh) |
| `today_charge_kwh` | REAL | IR 36 (×0.1) | Battery charge today (kWh) |
| `today_discharge_kwh` | REAL | IR 37 (×0.1) | Battery discharge today (kWh) |
| `today_consumption_kwh` | REAL | IR 35 (×0.1) | Home consumption today (kWh) |
| `grid_power` | INTEGER | IR 30 | Instantaneous grid power (W, signed) |

### History API

`GET /api/history?range=24h&fields=soc,battery_power&offset=0`

Returns time-bucketed aggregated values per field. **Cumulative counter fields**
(`today_*_kwh`) use MAX aggregation (preserves monotonically increasing counter
values). All other fields use AVG.

```json
{
  "ok": true,
  "data": {
    "soc": [{ "t": 1717000000000, "v": 75 }, ...],
    "battery_power": [{ "t": 1717000000000, "v": 800 }, ...]
  }
}
```

Buckets are aligned to hour/day boundaries. Query parameters:

| Range | Bucket | `range` value |
|---|---|---|
| 1 hour | 30 seconds | `1h` |
| 6 hours | 60 seconds | `6h` |
| 24 hours | 5 minutes | `24h` |
| 7 days | 30 minutes | `7d` |
| 30 days | 2 hours | `30d` |
| 6 months | 12 hours | `6m` |
| 1 year | 24 hours | `1y` |

### Cost charts

The cost charts (Import Cost, Export Income) on the History page use deltas of
the MAX-aggregated `today_import_kwh`/`today_export_kwh` values. Each delta is
classified as peak or off-peak based on the configured tariff time windows and
multiplied by the appropriate rate. See AGENTS.md for full sanitization details.

## GivEnergy Modbus Protocol

### Frame format (proprietary MBAP variant)

```
Bytes 0–1:   Transaction ID    — fixed 0x5959
Bytes 2–3:   Protocol ID       — fixed 0x0001
Bytes 4–5:   Length             — bytes after this field (+1 vs standard Modbus)
Byte  6:     Unit ID            — fixed 0x01
Byte  7:     Function ID        — 0x02 (transparent message)
Bytes 8–17:  Dongle serial      — 10 bytes, Latin-1
Bytes 18–25: Padding            — big-endian u64, value 8
Byte  26:    Device address     — 0x11 (writes), 0x32 (reads)
Byte  27:    Inner function     — 0x03 (read holding), 0x04 (read input), 0x06 (write single)
Bytes 28+:   Inner payload
Last 2 bytes: CRC/check
```

### Write protocol

Per the [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) reference library:

- Function code **6** (Write Single Register), one register per request
- Device address **0x11** (inverter setup address)
- Check field: `CrcModbus(function_code + register + value)`
- Exception code 67 = dongle busy; retry up to 6 times with 2s delay

### Read protocol

- Function code **3** (Read Holding Registers) or **4** (Read Input Registers)
- Device address **0x32** (BMS/poll address)
- Reads in blocks of 60 registers, aligned on 60-register boundaries
- 10-byte inverter serial prepended to response payload (skipped during decode)
- Response CRC validation is lenient — logged but not rejected (algorithm unknown per reference library)

### Key register addresses

| Register | Type | Description |
|---|---|---|
| IR 0 | Input | Inverter status (0=waiting, 1=normal, 2=warning, 3=fault) |
| IR 1–2 | Input | PV1/PV2 voltage (×0.1 V) |
| IR 5 | Input | Grid voltage (×0.1 V) |
| IR 8–9 | Input | PV1/PV2 current (×0.1 A) |
| IR 18, 20 | Input | PV1/PV2 power (W) |
| IR 30 | Input | Grid power (signed, +export/−import) |
| IR 50 | Input | Battery voltage (×0.01 V) |
| IR 51 | Input | Battery current (signed, ×0.01 A) |
| IR 52 | Input | Battery power (signed, +charging/−discharging) |
| IR 56 | Input | Battery temperature (×0.1 °C) |
| IR 59 | Input | Battery SOC (%) |
| IR 60–119 | Input | BMS data (cell voltages, temps) at device 0x32 |
| HR 20/27 | Holding | Battery power mode (0=export, 1=eco) |
| HR 31–32 | Holding | Charge slot 2 start/end (HHMM) |
| HR 44–45 | Holding | Discharge slot 2 start/end (HHMM) |
| HR 50 | Holding | Active power rate |
| HR 56–57 | Holding | Discharge slot 1 start/end (HHMM) |
| HR 59 | Holding | Enable discharge (bool) |
| HR 94–95 | Holding | Charge slot 1 start/end (HHMM) |
| HR 96 | Holding | Enable charge (bool) |
| HR 110 | Holding | Battery SOC reserve (%) |
| HR 111 | Holding | Battery charge limit (%) |
| HR 112 | Holding | Battery discharge limit (%) |
| HR 116 | Holding | Charge target SOC (%) |

### Slot enabled/disabled logic

1. `decode_timeslot()` checks time values: value 60 or minute > 59 → disabled; 00:00–00:00 → disabled
2. After decoding all blocks, global `enable_charge` / `enable_discharge` flags override: if flag is false, all slots in that category are forced to `enabled: false`
3. This ensures the GUI reflects the actual inverter state even when individual register writes fail

### Battery mode derivation

```rust
match (eco, enable_discharge, reserve == 100) {
    (true,  false, false) => Eco,
    (true,  false, true)  => EcoPaused,
    (true,  true,  _)     => TimedDemand,
    (false, true,  _)     => TimedExport,
    (false, false, _)     => ExportPaused,
}
```

### Proposed Eco and timed-export design

This section records the intended resolution of issue #289. It is a design,
not a description of the current implementation.

#### Register semantics

The discharge controls are related but not interchangeable:

| Mechanism | Registers | Meaning |
|---|---|---|
| Eco / match demand | HR27 = 1 | Battery discharge follows home demand; it does not deliberately export surplus |
| Maximum-power discharge | HR27 = 0 | Battery runs at its configured discharge rate; output beyond home demand is exported |
| DC discharge schedule | HR59 + HR44/45, HR56/57 and extended slots | Arms the inverter's discharge windows |
| Timed Discharge / pause discharge | HR318 = 2, HR319/320 | Prevents discharge during the pause window |

The established local-Modbus interpretation is:

```text
HR27=1, HR59=0  Eco
HR27=1, HR59=1  Timed Demand
HR27=0, HR59=1  Timed Export
HR27=0, HR59=0  Export Paused
```

GivTCP deliberately uses this conservative model. Its Timed Demand command
writes HR27=1 and HR59=1; Timed Export and Force Export always write HR27=0
and HR59=1. GivTCP does not query the cloud capability flag and has no
model/firmware table for it.

Household demand does not define whether Eco is enabled. During maximum-power
export the house consumes the power it needs from the local AC bus and only
surplus reaches the grid. For example, 3.6 kW battery output with 1 kW home
load produces approximately 2.6 kW export. HR27 controls the battery's
_discharge target_, not whether the home is supplied first.

#### `full-power-discharge-in-eco-mode`

GivEnergy exposes `full-power-discharge-in-eco-mode` as cloud metadata rather
than a readable local Modbus register:

- When present, a scheduled DC discharge runs at full power during its slot
  even while HR27 remains 1. The firmware temporarily overrides Eco for the
  slot and Eco can remain the configured baseline outside it.
- When absent, HR27=1 limits scheduled discharge to matching demand. A
  controller must write HR27=0 to guarantee full-power export.
- When unknown, HEM must not infer support solely from inverter family or
  firmware until a verified capability table exists. Automatic probing is
  unsafe and unreliable because the result depends on SOC, load, export
  limits, generation and the current schedule.

A known-capable inverter may use the native combination HR27=1, HR59=1 with
its slots permanently armed. All other devices need explicit boundary
management if the user wants normal Eco between export windows.

#### App-managed Timed Export

For absent or unknown capability, HEM should treat Timed Export as a temporary
override of an Eco baseline rather than leaving maximum-power mode selected
all day. The desired slots are persisted in HEM and, where firmware permits,
remain programmed on the inverter continuously.

Outside an export window:

```text
HR59 = 0  disarm scheduled discharge
HR27 = 1  select Eco / match demand
```

At window entry:

```text
HR27 = 0  select maximum-power discharge
HR59 = 1  arm the already-programmed export slot
```

At window exit:

```text
HR59 = 0  stop scheduled discharge first
HR27 = 1  restore Eco
```

The slot remains an inverter-side safety boundary. If HEM stops during an
export, the inverter slot still ends the discharge. HR27 may remain 0 and hold
the battery afterward, so startup and reconnect handling must detect that an
export window is no longer active and repair HR59=0 followed by HR27=1.
Missing the entry transition means full-power export does not start, so the UI
must state that this mode requires HEM to remain running.

Multiple and overnight slots are evaluated using inverter-local time. Entry
and exit transitions must be idempotent, survive reconnects, and avoid
re-queueing the same writes every poll.

#### Firmware that re-arms HR59

Some Gen3 firmware reasserts HR59 when any discharge slot remains non-zero.
The preferred permanent-slot approach must therefore be confirmed from
readback:

1. At window exit, write HR59=0 and HR27=1.
2. Observe subsequent polls for an unsolicited HR59 return to 1.
3. If HR59 remains 0, keep the slots on the inverter permanently.
4. If HR59 reasserts, do not fight it with continuous writes. Persist the
   desired schedule in HEM, clear the physical slots outside export windows,
   and restore them immediately before HR27=0/HR59=1 at entry.

On the fallback path the inverter slots are cleared only because that firmware
makes true Eco impossible while they remain populated. The persisted HEM
schedule is still the user-visible source of truth. A backed-up or app-managed
slot must remain visible as **Configured** in the Control page even when the
live inverter slot is temporarily zero; it must not become an empty editor or
disable the Timed Export control.

#### Interaction with HR318 Timed Discharge

HR318 is an independent pause gate and is not part of the HR27/HR59 export
transition:

```text
HR318=0  pause disabled
HR318=1  pause charging
HR318=2  pause discharging
HR318=3  pause charging and discharging
```

HEM presents HR318=2 as Timed Discharge. For a visible demand window of
03:00-04:00, HR319/320 contains the inverse pause window 04:00-03:00. HR318
does not initiate discharge; it only permits discharge inside the visible
window and blocks it outside.

Ordinary scheduled Timed Export must respect HR318:

| Export window | HR318 is currently blocking discharge | Effective behaviour |
|---|---|---|
| Outside | No | Eco |
| Outside | Yes | Eco configured, battery discharge paused |
| Inside | No | HR27=0 and HR59=1; maximum-power export |
| Inside | Yes | Export remains scheduled but is blocked by Pause Discharge |

Scheduled export must not silently clear HR318. If an export slot overlaps an
active pause interval, the UI should report **Blocked by Pause Discharge**.
If it overlaps the allowed Timed Discharge window, export may run normally.

A manual Force Discharge is deliberately different. It is an explicit
override and may temporarily disable battery pause, but it must capture
HR318, HR319 and HR320 first and restore all three when the force action ends.
Thermal and hardware safety protection always has higher priority than manual
or scheduled discharge.

The intended priority is:

1. Hardware and thermal safety
2. Explicit Pause Discharge / HR318
3. Manual Force Charge or Force Discharge (with captured pause-state restore)
4. Scheduled Timed Export
5. Timed Charge
6. Eco baseline

#### Battery-mode presentation

The Control page must not present Eco, Timed Charge, Timed Discharge and Timed
Export as four equivalent binary modes. Eco is a baseline; the others are
armed schedules or temporary overrides. A single cyan highlight currently
conflates **configured**, **armed** and **active now**.

The UI should separate:

- **Baseline**: normally `Eco`.
- **Current behaviour**: Eco, charging, demand discharge, export, paused or a
  safety override.
- **Schedules**: Off, Configured, Armed, Active now, Blocked or Error.

Eco has three presentation states:

| State | Presentation |
|---|---|
| Eco controls discharge now | **Eco — Active** |
| Eco is the configured baseline but Timed Export, Force Discharge or pause currently owns behaviour | **Eco — Temporarily overridden** |
| Eco was explicitly disabled and will not be restored | **Eco — Off** |

During an active Timed Export window, Eco should therefore not appear simply
active or permanently off. Show:

```text
Baseline: Eco — temporarily overridden
Current behaviour: Timed Export — exporting now
Home load is supplied first; surplus power is exported.
Eco resumes at 19:00.
```

This remains the correct presentation on a flag-capable inverter even if the
raw HR27 value stays 1: the firmware's scheduled full-power behaviour, not Eco
match-demand, currently controls battery output. Raw HR27/HR59 values belong
in developer diagnostics, not in the user-facing meaning of the indicator.

Timed Charge and Timed Export indicators must distinguish a future armed slot
from current activity. Timed Export is **Active now** only when the current
time is in an enabled export slot, the export transition is confirmed, HR318
is not blocking it, and telemetry is consistent with discharge/export. HR59
alone is not proof of Timed Export because HR27=1/HR59=1 is Timed Demand on
ordinary firmware.

#### Required verification

Implementation must add deterministic tests for:

- Normal, overnight and adjacent export-slot entry/exit transitions.
- Write ordering: HR27 then HR59 at entry; HR59 then HR27 at exit.
- Restart and reconnect inside and outside an export window.
- Permanent-slot operation when HR59 remains off outside a window.
- Detection and fallback when firmware reasserts HR59.
- Persistence and display of configured slots while physical slots are clear.
- HR318 blocking, non-overlapping and allowed-window overlap cases.
- Force Discharge capture/disable/restore of HR318-320.
- Eco **Active**, **Temporarily overridden** and **Off** presentation.
- Timed Export **Configured**, **Active now** and **Blocked** presentation.
- Fixed clocks for every time-window test.

## Testing

101 Rust unit tests across all modules. No frontend tests.

```bash
cd src-tauri && cargo test
```

Key test modules:

- `decoder::tests` — full snapshot decode, battery state derivation, timeslot handling
- `encoder::tests` — command encoding, whitelist validation, range checks
- `framer::tests` — frame encode/decode roundtrip, CRC, header validation
- `client::tests` — register parsing, error handling
- `registers::tests` — HHMM codec, poll block coverage, register address verification

## Build & Release

### Development

```bash
npm install
cd src-tauri && cargo tauri dev
```

### Production build

```bash
npm run build          # Typecheck + bundle frontend
cd src-tauri
cargo tauri build      # Build native desktop app
```

### CI/CD

GitHub Actions workflow (`.github/workflows/build.yml`):

- Triggers on tag push (`v*`) or manual dispatch
- Builds for: macOS (aarch64 + x86_64), Linux (x86_64), Windows (x86_64)
- macOS DMG is customized: removes `/Applications` symlink (breaks on macOS 26.5+),
  adds `README.txt` with install instructions
- Creates GitHub Release with binaries and installers attached

## Configuration

`~/.givenergy-local/settings.json`:

```json
{
  "host": "192.168.1.36",
  "port": 8899,
  "serial": "",
  "poll_interval": 60,
  "auto_connect": true
}
```

Leave `serial` empty for auto-discovery from the dongle's first response frame.

## API Reference

| Method | Endpoint | Description |
|---|---|---|
| GET | `/api/snapshot` | Latest inverter snapshot (JSON) |
| GET | `/api/status` | Connection state + poll health |
| GET/POST | `/api/settings` | Read/update settings (connection, tariffs, alerts, weather, automation …) |
| GET | `/api/history`, `/api/history/summary` | Aggregated time-series (`?range=,fields=,offset=`) + period summary |
| GET | `/api/report` | Power-page consumption report data |
| POST | `/api/control/mode`, `/eco`, `/timed-charge`, `/timed-export` | Battery mode selection |
| POST | `/api/control/charge-slot`, `/discharge-slot` | Configure schedule slots (incl. `target_soc`) |
| POST | `/api/control/reserve`, `/charge-rate`, `/discharge-rate`, `/export-limit`, `/eps` | Limits and SOC reserve |
| POST | `/api/control/pause`, `/unpause`, `/force-charge`, `/force-discharge` | Manual overrides |
| POST | `/api/control/calibration`, `/reboot`, `/sync-clock` | Maintenance actions |
| GET/POST | `/api/auto-winter`, `/charging-mode`, `/adaptive-charge`, `/load-limiter`, `/temperature-limiter`, `/discharge-floor` | Automation configuration |
| GET/POST | `/api/cosy`, `/api/agile` | Octopus Cosy / Agile tariff automation |
| GET/POST | `/api/octopus/*` | Octopus account data (status, sync, history, summary, comparison) |
| GET | `/api/forecast`, `/api/forecast/plan` | 48 h forecast + overnight charge recommendation (#283) |
| GET/POST | `/api/weather`, `/api/weather/backfill` | Local weather config + history backfill |
| GET/POST | `/api/alerts`, `/api/alerts/test` | Alert thresholds + test delivery |
| GET | `/api/discover`, `/api/evc/discover` | Network scans (inverter, EV charger) |
| GET | `/api/evc/status` | EV charger reachability + cached snapshot |
| GET | `/api/logs`, `/api/log-level`, `/api/latest-version` | Dev console logs, log level, update check |
| POST | `/api/reconnect` | Force a reconnect |
| WS | `/ws` | Real-time snapshot + connection state stream |

## Docker Deployment

The app can run headless in Docker for always-on server deployments.

### Files

| File | Purpose |
|------|---------|
| `Dockerfile` | Multi-stage build: node (frontend) → rust (binary) → debian-slim (runtime) |
| `docker-compose.yml` | Production config: host networking, persistent volume |
| `.dockerignore` | Excludes `node_modules/`, `target/`, `dist/`, `.git/`, `.env` |

### Build and run

```bash
docker compose up -d          # start
docker compose build          # rebuild after code changes
docker compose down           # stop and remove (data persists in volume)
docker logs givenergy-local   # view logs
```

### Architecture notes

- **Host networking** is required — the Modbus TCP client needs LAN access to the inverter at port 8899
- **Volume mount** `${HOME}/.givenergy-local:/root/.givenergy-local` persists `settings.json` and `history.db` across restarts
- The container runs `givenergy-local --headless` by default on port 7337
- Image size is ~1.5 GB due to GTK/WebKit runtime dependencies (Tauri links against them even in headless mode)
- `GIVENERGY_LOCAL_CONFIG_DIR` env var is set to `/root/.givenergy-local` inside the container

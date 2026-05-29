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
| `src/components/EnergyFlowDiagram.tsx` | Radial SVG with animated power flow lines |
| `src/components/BatteryPanel.tsx` | Per-battery-module cell voltage/temperature table |
| `src/pages/ControlPage.tsx` | Schedule slots, mode selector, SOC/limit sliders |
| `src/pages/SettingsPage.tsx` | Connection config, discovery, about section |

### State management

Zustand store (`useInverterStore`):

```typescript
{
  snapshot: InverterSnapshot | null,
  connectionState: 'connected' | 'disconnected',
  connectedHost: string | null,
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
├── lib.rs              Tauri setup, spawns server + poll task
├── main.rs             Tauri builder entry point
├── inverter/
│   ├── mod.rs          Re-exports
│   ├── model.rs        InverterSnapshot, ScheduleSlot, BatteryMode, BatteryState
│   ├── decoder.rs      Register → snapshot decoder, timeslot logic, enable flag gating
│   ├── encoder.rs      ControlCommand → RegisterWrite encoder, whitelist validation
│   ├── poll.rs         Poll loop: write queue → register reads → snapshot broadcast
│   └── discovery.rs    Network scan, subnet inference, serial auto-detect
├── modbus/
│   ├── mod.rs          Re-exports
│   ├── client.rs       ModbusClient: connect, read, write (FC6), stale frame drain
│   ├── framer.rs       GivEnergy frame encode/decode (proprietary MBAP variant)
│   └── registers.rs    Register addresses, poll blocks, safe-write list, HHMM codec
├── server/
│   ├── mod.rs          Axum router, server startup (graceful error handling)
│   ├── api.rs          REST endpoints (/api/control/*, /api/snapshot, /api/settings)
│   └── ws.rs           WebSocket handler, PollMessage broadcast
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

### Shared state (AppState)

```rust
pub struct AppState {
    pub latest_snapshot: Arc<Mutex<Option<InverterSnapshot>>>,
    pub connection_state: Arc<Mutex<ConnectionState>>,
    pub tx: broadcast::Sender<PollMessage>,
    pub settings: Arc<Mutex<PollSettings>>,
    pub pending_writes: Arc<Mutex<Vec<Vec<RegisterWrite>>>>,
    pub write_notify: Arc<Notify>,
}
```

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

## Testing

94 Rust unit tests across all modules. No frontend tests.

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
| GET/POST | `/api/settings` | Read/update connection settings |
| POST | `/api/control/mode` | Set battery mode (`{mode: "eco\|timed_demand\|timed_export\|pause"}`) |
| POST | `/api/control/charge-slot` | Configure charge slot (`{slot, enabled, start_hour, start_minute, end_hour, end_minute, target_soc}`) |
| POST | `/api/control/discharge-slot` | Configure discharge slot (same shape, no target_soc) |
| POST | `/api/control/reserve` | Set SOC reserve (`{soc: 4}`) |
| POST | `/api/control/charge-rate` | Set charge power limit (`{limit: 50}`) |
| POST | `/api/control/discharge-rate` | Set discharge power limit (`{limit: 50}`) |
| POST | `/api/control/pause` | Pause battery (sets SOC reserve to 100) |
| GET | `/api/discover` | Scan network for inverters |
| WS | `/ws` | Real-time snapshot + connection state stream |

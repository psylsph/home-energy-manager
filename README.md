# GivEnergy Local

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP — no cloud required.

![License](https://img.shields.io/badge/license-MIT-blue.svg)

## Features

- **Real-time monitoring** — solar generation, battery state, grid import/export, home consumption
- **Battery per-module breakdown** — individual cell voltages, temperatures, SOC, cycle count per physical battery
- **Energy flow diagram** — live radial visualisation of power flows
- **Schedule management** — charge/discharge time slots with SOC targets
- **Battery mode control** — Eco, Timed Demand, Timed Export, Pause
- **Auto-discovery** — just enter the inverter IP; serial number is detected automatically
- **Zero cloud dependency** — all communication is over your local network via Modbus TCP

## Stack

| Layer | Technology |
|---|---|
| Frontend | React 19, TypeScript, Vite 8, Tailwind CSS 4, Zustand, Recharts |
| Desktop shell | Tauri 2 |
| Backend | Axum HTTP + WebSocket server (embedded, port 7337) |
| Protocol | Custom GivEnergy Modbus TCP client (port 8899) |
| Testing | Rust unit tests (94 passing) |

## Screenshots

*Coming soon*

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (1.77+)
- [Node.js](https://nodejs.org/) (20+)
- A GivEnergy inverter with a WiFi/Ethernet data adapter on your local network

### Development

```bash
# Install frontend dependencies
npm install

# Run in development mode (Tauri window + Vite HMR)
cd src-tauri && cargo tauri dev
```

The app starts an embedded HTTP/WebSocket server on port 7337. Open `http://localhost:7337` in a browser for the web UI, or use the Tauri desktop window.

### Production Build

```bash
npm run build          # Typecheck + bundle frontend
cd src-tauri
cargo tauri build      # Build native desktop app
```

### Testing

```bash
# Frontend typecheck
npm run build

# Rust unit tests
cd src-tauri && cargo test
```

## Architecture

```
┌─────────────┐     HTTP/WS      ┌──────────────┐    Modbus TCP    ┌───────────┐
│  React UI   │ ◄──────────────► │  Axum server │ ◄──────────────► │ Inverter  │
│  (browser)  │    port 7337     │  (embedded)  │    port 8899     │ dongle    │
└─────────────┘                  └──────────────┘                  └───────────┘
```

- `src/` — React frontend. Pages: **Status**, **Battery**, **History**, **Control**, **Settings**
- `src-tauri/src/` — Rust backend
  - `inverter/` — data model, register decode/encode, discovery, poll loop
  - `modbus/` — TCP client, GivEnergy frame protocol, register map
  - `server/` — Axum REST API (`/api/*`) + WebSocket (`/ws`)
  - `settings/` — persisted config (~/.givenergy-local/settings.json)

The frontend talks exclusively to the local Axum server — never directly to the inverter.

## API

| Method | Endpoint | Description |
|---|---|---|
| GET | `/api/snapshot` | Latest inverter snapshot |
| GET/POST | `/api/settings` | Read/update connection settings |
| POST | `/api/control/mode` | Set battery operating mode |
| POST | `/api/control/charge-slot` | Configure charge schedule |
| POST | `/api/control/discharge-slot` | Configure discharge schedule |
| POST | `/api/control/reserve` | Set battery SOC reserve |
| POST | `/api/control/charge-rate` | Set charge power limit |
| POST | `/api/control/discharge-rate` | Set discharge power limit |
| POST | `/api/control/pause` | Pause battery |
| GET | `/api/discover` | Scan network for inverters |
| WS | `/ws` | Real-time snapshot + connection state stream |

## Register Map

GivEnergy register addresses sourced from the [givenergy-modbus](https://github.com/andrewlesakowski/givenergy-modbus) reference library. Key registers:

**Input Registers** (telemetry, read-only):

| Register | Scale | Description |
|---|---|---|
| 0 | — | Status (0=waiting, 1=normal, 2=warning, 3=fault) |
| 1, 2 | ×0.1 V | PV1/PV2 voltage |
| 5 | ×0.1 V | Grid voltage |
| 8, 9 | ×0.1 A | PV1/PV2 current |
| 13 | ×0.01 Hz | Grid frequency |
| 18, 20 | W | PV1/PV2 power |
| 30 | W (signed) | Grid power (+export/−import) |
| 50 | ×0.01 V | Battery voltage |
| 51 | ×0.01 A (signed) | Battery current |
| 52 | W (signed) | Battery power (+charging/−discharging) |
| 56 | ×0.1 °C | Battery temperature |
| 59 | % | Battery SOC |

**Battery BMS** (device 0x32, input registers 60-119):

| Register | Scale | Description |
|---|---|---|
| 60-75 | mV | Cell voltages (up to 16 cells) |
| 76-79 | ×0.1 °C | Cell group temperatures |
| 82-83 | mV (uint32) | Total pack voltage |
| 97 | — | Number of cells |
| 98 | — | BMS firmware version |
| 100 | % | SOC |
| 103 | ×0.1 °C | Max cell temperature |
| 110-114 | Latin-1 | Serial number |

## Configuration

Settings are stored in `~/.givenergy-local/settings.json`:

```json
{
  "host": "192.168.1.36",
  "port": 8899,
  "serial": "",
  "poll_interval": 60,
  "auto_connect": true
}
```

Leave `serial` empty to auto-discover from the dongle's first response.

## License

MIT — see [LICENSE](./LICENSE).

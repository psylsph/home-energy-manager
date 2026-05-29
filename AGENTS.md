# GivEnergy Local

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## Stack

- **Frontend**: React 19 + TypeScript + Vite 8 + Tailwind CSS 4 + Zustand + Recharts + React Router 7
- **Backend**: Tauri 2 desktop shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: Custom Rust TCP client to GivEnergy data adapter (port **8899**)
- **Testing**: Rust unit tests only (no frontend tests, no integration tests)

## Prerequisites

- **Node.js** + npm
- **Rust** toolchain (`rustup`)
- **Tauri CLI**: `cargo install tauri-cli`

## Commands

| Command | Action |
|---|---|
| `npm run dev` | Vite dev server on port 5173 |
| `npm run build` | `tsc -b && vite build` (full typecheck + bundle) |
| `npm run lint` | `eslint .` |
| `npm run preview` | `vite preview` |
| `cargo test` (in `src-tauri/`) | Run all Rust unit tests (94 tests) |
| `cargo tauri dev` | Dev mode with Tauri window + Vite + hot-reload |
| `cargo tauri build` | Production build of the desktop app |

Order for full verification: `npm run lint` → `npm run build` (typechecks) → `cargo test` in `src-tauri/`.

## Architecture

### Frontend (`src/`)

React app. Entrypoint: `src/main.tsx`.

- **Pages**: `StatusPage` (dashboard + energy flow), `BatteryPage` (cell-level detail), `HistoryPage` (charts), `ControlPage` (schedules, modes, limits), `SettingsPage` (connection config, about)
- **Components**: `EnergyFlowDiagram` (radial SVG power flow), `BatteryPanel` (per-module cell data), `SummaryTiles` (power stats)
- **Hooks**: `useWebSocket` — connects to `/ws`, reconnects on drop, fetches initial snapshot via REST
- **Lib**: `api.ts` (fetch helpers), `format.ts` (power/voltage/temp formatters), `types.ts` (InverterSnapshot etc.)
- **State**: Zustand store (`useInverterStore`) holds `snapshot`, `connectionState`, `connectedHost`
- **Version**: Injected at build time via `__APP_VERSION__` (defined in `vite.config.ts`, declared in `src/env.d.ts`)

Frontend talks exclusively to the local Axum server — never directly to the inverter.

### Backend (`src-tauri/src/`)

- **`lib.rs`** — Tauri app setup; spawns Axum server (port 7337) + Modbus polling loop
- **`inverter/`** — data model, register decode/encode, discovery, poll loop
  - `model.rs` — `InverterSnapshot`, `ScheduleSlot`, `BatteryMode`, `BatteryState`
  - `decoder.rs` — converts raw register blocks into `InverterSnapshot`; applies global `enable_charge`/`enable_discharge` flags to slot states
  - `encoder.rs` — translates `ControlCommand` enum into `RegisterWrite` lists (whitelist-validated)
  - `poll.rs` — main polling loop: drain pending writes → read registers → broadcast snapshot; uses `Notify` for immediate write execution
  - `discovery.rs` — network scanning, subnet inference, serial auto-detection
- **`modbus/`** — GivEnergy Modbus TCP protocol
  - `client.rs` — `ModbusClient`: connect, read registers, write single register (FC6), stale frame drain
  - `framer.rs` — proprietary frame encode/decode (MBAP header + transparent sub-frame + CRC); response CRC validation is lenient (logged, not rejected)
  - `registers.rs` — register addresses, poll block definitions, safe-write whitelist, HHMM encode/decode
- **`server/`** — Axum HTTP layer
  - `api.rs` — REST endpoints for control commands; queues writes to `AppState::pending_writes` and notifies poll loop
  - `ws.rs` — WebSocket endpoint streaming `PollMessage` (snapshot or connection state)
  - `mod.rs` — router setup, server startup (graceful bind failure, no panic)
- **`settings/`** — persisted JSON config (`~/.givenergy-local/settings.json`)

### Shared state (`AppState`)

Central `Arc<Mutex<…>>`-based state shared between poll loop, API handlers, and WebSocket:

- `latest_snapshot` — most recent `InverterSnapshot`
- `connection_state` — `Connected` / `Disconnected`
- `pending_writes` — queue of `Vec<RegisterWrite>` batches from the API
- `write_notify` — `Notify` that wakes the poll loop immediately when writes are queued
- `settings` — live `PollSettings` (host, port, serial, interval)

## Modbus write protocol

Per the [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) reference library:

- **Function code 6** (Write Single Holding Register) — one register per request
- **Device address 0x11** (inverter setup address) — NOT 0x32 (BMS/poll address)
- **CRC/check**: `CrcModbus(function_code + register + value)` — computed per the reference library
- **Slot clearing**: write `0` (not sentinel 60) — `00:00–00:00` is treated as disabled
- **Retry policy**: 6 attempts with 2s delay for exception code 67 (dongle busy); fail fast and continue

Known limitation: register 32 (charge slot 2 end time) consistently returns exception 67 on some inverters despite being in the reference library's safe-write list. The system handles this gracefully — `enable_charge` flag still updates correctly.

## TypeScript quirks

- `verbatimModuleSyntax: true` — use `import type` for type-only imports
- `erasableSyntaxOnly: true` — no `enum`, no `namespace`, no `constructor parameter properties`
- `noUnusedLocals` / `noUnusedParameters` — both on; declarations must be used
- ESLint rule `react-hooks/set-state-in-effect` — do not call `setState` directly inside `useEffect`; use key-based remounting or derived values instead

## Rust testing

All tests are `#[cfg(test)]` unit tests inside each module. Run with:
```
cd src-tauri && cargo test
```
No integration tests or test fixtures exist. The Modbus client tests use a mock TCP server.

## Build artifacts

- `dist/` — Vite output (frontend)
- `src-tauri/target/` — Rust build output
- `node_modules/.tmp/tsconfig.*.tsbuildinfo` — TypeScript incremental build info

## Release process

1. Bump version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`
2. Update `CHANGELOG.md`
3. Commit, tag (`vX.Y.Z`), push tag
4. GitHub Actions workflow (`.github/workflows/build.yml`) builds for macOS (ARM + x64), Linux, Windows and creates a GitHub Release with binaries attached

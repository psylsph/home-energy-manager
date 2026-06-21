# Home Energy Manager

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## General rules

- **Never close GitHub issues or PRs** without explicit permission from the project owner.

## Stack

- **Frontend**: React 19 + TypeScript + Vite 9 + Tailwind CSS 4 + Zustand + Recharts + React Router 7
- **Backend**: Tauri 2 desktop shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: Custom Rust TCP client to GivEnergy data adapter (port **8899**) aligned with [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) reference library and [GivTCP](https://github.com/dewet22/giv_tcp)
- **Testing**: Rust unit tests (244) + integration tests with mock TCP server + Playwright end-to-end tests. Local-only E2E use [GivEnergy Simulator](https://github.com/psylsph/givenergy-simulator)
- **References**: Local clones at `~/repos/givenergy-modbus` and `~/repos/giv_tcp` are source of truth for register layout, slot maps, slave addressing, command encoding

## Prerequisites

- **Node.js** + npm
- **Rust** toolchain (stable; `rustup default stable`)
- **Tauri CLI**: `cargo install tauri-cli`

## Commands

| Command | Action |
|---|---|
| `npm run dev` | Vite dev server on port 5173 |
| `npm run build` | `tsc -b && vite build` (full typecheck + bundle) |
| `npm run lint` | `eslint .` |
| `npm run preview` | `vite preview` |
| `cargo test` (in `src-tauri/`) | Run all Rust unit tests |
| `cargo clippy` (in `src-tauri/`) | Run Rust linter |
| `cargo tauri dev` | Dev mode with Tauri window + Vite + hot-reload |
| `cargo tauri build` | Production build |
| `docker build .` | Docker container build |
| `npm run test:e2e` | Playwright E2E (requires `npm run build` + `cargo build --release` first) |
| `npm run test:local` | Local-only E2E using simulator at `~/repos/givenergy-simulator/target/release/sim-api` |

Full verification order: `cargo clippy` → `npm run lint` → `npm run lint:md` → `npm run build` → `cargo test` → `npm run test:e2e` → `docker build .`

## Linting rules

### Rust (clippy)

All clippy warnings must be fixed. Common patterns: `empty_line_after_doc_comments`, `field_reassign_with_default`, `manual_flatten`, `match_like_matches_macro`, `derivable_impls`, `new_without_default`, `same_item_push`, `manual_clamp`. Run: `cd src-tauri && cargo clippy`

### TypeScript / ESLint

- `verbatimModuleSyntax: true` — use `import type` for type-only imports
- `erasableSyntaxOnly: true` — no `enum`, no `namespace`, no constructor parameter properties
- `noUnusedLocals` / `noUnusedParameters` — both on
- `react-hooks/set-state-in-effect` — do not call `setState` directly inside `useEffect`; use key-based remounting or derived values instead

Run: `npm run lint`

### Markdown

Run `npx markdownlint '**/*.md' --ignore node_modules` after significant .md edits.

## Architecture

### Frontend (`src/`)

Entrypoint: `src/main.tsx`.

- **Pages**: `StatusPage`, `BatteryPage`, `HistoryPage`, `ControlPage` (model-aware rate scaling, slot-labelling warnings), `InverterPage`, `SettingsPage`, `LogsPage` (developer mode only)
- **Components**: `EnergyFlowDiagram` (radial SVG power flow), `BatteryPanel`, `SummaryTiles`
- **Hooks**: `useWebSocket` — connects to `/ws`, reconnects on drop, fetches snapshot via REST
- **Lib**: `api.ts` (fetch helpers), `format.ts` (formatters), `types.ts` (types)
- **State**: Zustand store (`useInverterStore`) — snapshot, connectionState, connectedHost, developerMode (persisted to localStorage)
- **Version**: `__APP_VERSION__` (from `vite.config.ts`)

Frontend talks exclusively to the local Axum server — never directly to the inverter.

### Backend (`src-tauri/src/`)

- **`lib.rs`** — Tauri app setup + headless CLI; spawns Axum server + Modbus polling loop. Two independent tracing layers: `fmt` layer to stdout (level WARN, override via `RUST_LOG`) and `LogCaptureLayer` into in-memory `LogRing` for dev console (level WARN, runtime-adjustable via `PUT /api/log-level`).
- **`history/`** — SQLite-backed history (`~/.givenergy-local/history.db`). `HistoryDb` wrapper, schema migration, `insert_reading()`, aggregated `query_history()` with time-bucket AVG (or MAX for cumulative fields).
- **`inverter/`** — data model, register decode/encode, discovery, poll loop
  - `model.rs` — `InverterSnapshot`, `ScheduleSlot`, `BatteryMode`, `BatteryState`, `DeviceType` enum (Gen1-4, AC-coupled, three-phase, AIO, HV Gen3/4, EMS) with model-aware helpers for slave addresses, poll blocks, slot counts, battery protocol selection.
  - `decoder.rs` — converts raw register blocks into `InverterSnapshot`; per-block decoders for holding registers 0-59, 60-119, 240-299 (extended 10-slot schedules), 300-359 (AC config), 1080-1124 (three-phase config).
  - `encoder.rs` — translates `ControlCommand` into whitelist-validated `RegisterWrite` lists. Model-specific commands for AC-coupled and three-phase limits.
  - `poll.rs` — main polling loop: drain writes → read registers → sanitize → broadcast snapshot. Features: dongle memory-leak fingerprint detection, model-aware slave address switching, carry-forward for optional blocks, two battery protocols (LV at 0x32+; HV at 0xA0→0x70+/0x50+), grace-period median-of-3 baseline hardening, derived three-phase battery fields.
  - `discovery.rs` — network scanning with GivEnergy Modbus protocol verification (validates 0x5959 magic header).
- **`modbus/`** — GivEnergy Modbus TCP protocol
  - `client.rs` — `ModbusClient`: connect, read registers, write single register (FC6), stale frame drain, heartbeat handling (echoes dongle heartbeats). Default slave address `0x11`. `read_all_with_extras()` decides optional blocks by device type.
  - `framer.rs` — proprietary frame encode/decode (MBAP header + transparent sub-frame + CRC)
  - `registers.rs` — register addresses, poll block definitions, safe-write whitelist, HHMM encode/decode. Standard blocks: `IR(0,60)`, `HR(0,60)`, `HR(60,60)`, `IR(180,4)` (alternative battery lifetime/daily totals for Gen1 Hybrid). Per-battery BMS reads (`BATTERY_1_POLL_BLOCK`, `BATTERY_POLL_BLOCK` = `IR(60,60)`) and HV stack reads (`HV_BCU_POLL_BLOCK` = `IR(60,60)` at device `0x70+`, BMU blocks at `0x50+`) are polled separately, not part of `STANDARD_POLL_BLOCKS`. Optional blocks (conditionally polled by device type): `EXTENDED_SLOTS_BLOCK` (HR 240-299), `AC_CONFIG_BLOCK` (HR 300-359), `THREE_PHASE_HIGH_CONFIG_BLOCK` (HR 1000-1079), `THREE_PHASE_CONFIG_BLOCK` (HR 1080-1124), seven `THREE_PHASE_INPUT_BLOCKS` (IR 1000-1413), five `GATEWAY_INPUT_BLOCKS` (IR 1600-1859).
- **`server/`** — Axum HTTP layer: `api.rs` (REST endpoints), `ws.rs` (WebSocket snapshot stream), `logs.rs` (LogRing + `GET /api/logs`), `mod.rs` (router + graceful bind)
- **`settings/`** — persisted JSON config (`~/.givenergy-local/settings.json`)
- **`alerts/`** — alert evaluation engine + push notifications. Evaluates each sanitized `InverterSnapshot` against user thresholds (battery temperature high/low, battery SOC high/low, solar-clipping ceiling, inverter battery-warning flag, grid offline) with per-type cooldown and consecutive-read confirmation, then delivers via the **Telegram Bot API** and/or **ntfy.sh** (including self-hosted ntfy). Also generates/sends the daily consumption report and polls Telegram for `/status`, `/today`, `/report` commands. **This covers GitHub issue #85 (critical-condition notifications) — implemented as Telegram + ntfy push notifications rather than email.**

### Shared state (`AppState`)

`Arc<Mutex<…>>` shared between poll loop, API handlers, and WebSocket: `latest_snapshot`, `connection_state`, `pending_writes`, `write_notify` (wakes poll loop), `settings`, `history`, `log_ring` (2000-entry ring buffer).

## Data sanitization (register corruption defense)

GivEnergy dongle frequently returns corrupted register values. The sanitizer in `poll.rs` defends with multiple layers:

### Absolute range checks (always active)

| Field | Range | Notes |
|---|---|---|
| `today_*_kwh` | 0–200 kWh | Catches 245, 275, 879, 1010 spikes |
| Battery power | ±10 kW | Residential limit |
| Grid power | ±15 kW | 100A fuse ≈ 23 kW |
| Solar power | 0–10 kW | |
| Home power | 0–15 kW | EV charging margin |
| Grid voltage | 180–280 V | |
| Grid frequency | 45–55 Hz | |
| Inverter temp | -20–100 °C | |
| Battery temp | -20–80 °C | |
| Battery module voltage | 0–500 V | LV (~48V) to HV (~345V) |
| SOC | 0–100 | Also rejects SOC=0 with live power, SOC=100 while fast-charging |

### Delta checks (after 3-reading grace period)

- Monotonic increase: `today_*_kwh` must not decrease (except midnight rollover)
- Time-based rate limit: `max_increase = elapsed_hours × 10 kW + 1 kWh`
- Jitter tolerance: decreases < 0.15 kWh accepted as dongle precision noise
- Near-zero prev: tighter time-aware ceiling applied instead of skip-open

### Connect sequence

```
Connect → 500ms delay → drain TCP → 3× warmup reads (discarded)
→ clear latest_snapshot → 3 grace readings (absolute check only)
  └─ cumulative counters median-of-3 on final grace reading
→ full absolute + delta checks
```

### History aggregation

History API uses MAX (not AVG) for cumulative counters (`today_*_kwh`) — AVG understates monotonic values. Frontend `removeSpikes()` in `HistoryPage.tsx` applies a post-query spike filter.

## Modbus write protocol

- **FC6** (Write Single Holding Register) — one register per request
- **Default device address `0x11`** — switches to `0x31` for AC-coupled/Gen1 after detection
- **CRC**: `CrcModbus(function_code + register + value)`
- **Slot clearing**: write `0` (`00:00–00:00` = disabled)
- **Retry**: 6 attempts with 2s delay on exception 67 (dongle busy)

### Model-specific write targets

| UI control | DC hybrid (Gen2/Gen3+) | AC-coupled | Three-phase / HV |
|---|---|---|---|
| Charge power limit | HR111 (0-50%) | HR313 (1-100%) | HR1110 (1-100%) |
| Discharge power limit | HR112 (0-50%) | HR314 (1-100%) | HR1108 (1-100%) |
| Battery SOC reserve | HR110 | HR110 | HR1109 |
| Charge target SOC | HR116 | HR116 | HR1111 |

The API routes inspect `device_type` to choose the right command. The frontend (`ControlPage.tsx`) picks the correct register max (50 vs 100) and display formula based on device type code.

Known limitation: register 32 (charge slot 2 end time) consistently returns exception 67 on some inverters despite being in the safe-write list. `enable_charge` flag still updates correctly.

## Battery power sign convention

HEM convention (uniform across device families and the frontend):
**`battery_power` positive = discharging, negative = charging.** The frontend
labels (`PowerPage.tsx`: `value > 0 ? 'Discharging' : 'Charging'`), the
`BatteryState` enum, history charting, and the gateway grid-power derivation
all assume this.

| Path | Raw register | Raw wire sign | Decode action |
|---|---|---|---|
| Single-phase | `p_battery` IR(52) | **+ = discharge** (reference) | verbatim |
| Three-phase / HV | `p_discharge - p_charge` (IR 1136-1139) | derived | computed, + = discharge |
| Gateway aggregate | `p_aio_total` IR(1702) | **+ = charging** (opposite!) | **negate** |
| Gateway per-AIO | `p_aioN_inverter` IR(1816-1818) | **+ = charging** (same as p_aio_total) | **negate** |

The gateway exception is confirmed by `GivTCP/read.py:1556`:
`Battery_Power = -GEInv.p_aio_total`. Forgetting the negate on gateway
inverts the battery arrow AND the derived grid power (since
`grid = solar + battery − home`), producing impossible readings like
solar 6 kW / home 0.6 kW with battery "discharging" 5.5 kW and
11 kW export (issue #78). See `decode_gateway_1660_1719` /
`decode_gateway_1780_1830` and the gateway sign-convention tests in
`decoder.rs` (`sign_convention_gateway_*`).

## Build artifacts

- `dist/` — Vite output
- `src-tauri/target/` — Rust build output
- `node_modules/.tmp/tsconfig.*.tsbuildinfo` — TypeScript incremental build info

## Headless server mode (Linux)

Run without Tauri window — just the Axum HTTP/WS server + Modbus poll loop.

```bash
npm run build
cd src-tauri && cargo build --release
./target/release/givenergy-local --headless
./target/release/givenergy-local --headless --port 8080
./target/release/givenergy-local --headless --dist /path/to/dist
```

`--dist` search order: `--dist` arg > `./dist/` (cwd) > `<exe_dir>/dist/` > `/usr/share/givenergy-local/dist/`. Runs API-only if no dist found.

## Schedule slot register layout

| Reference library name | Register | UI label |
|---|---|---|
| `charge_slot_1` | HR 94-95 | Slot 1 |
| `charge_slot_2` | HR 31-32 | Slot 2 |
| `discharge_slot_1` | HR 56-57 | Slot 1 |
| `discharge_slot_2` | HR 44-45 | Slot 2 |
| `charge_slot_3..10` | HR 246-268 | Slots 3-10 (Gen3, AIO, HV Gen3) |
| `charge_target_soc_1..10` | HR 242, 245, 248..269 | Per-slot target SOC (Gen3) |
| `discharge_slot_3..10` | HR 276-298 | Slots 3-10 (Gen3, AIO, HV Gen3) |
| `discharge_target_soc_1..10` | HR 272, 275, 278..299 | Per-slot target SOC (Gen3) |
| `charge_slot_2_gen3` | HR 243-244 | Gen3 extended copy of slot 2 |
| `3ph_charge_slot_1..2` | HR 1113-1116 | Three-phase slots 1-2 |
| `3ph_discharge_slot_1..2` | HR 1118-1121 | Three-phase slots 1-2 |
| `gateway_ems_charge_slots` | HR 2053-2071 | Gateway / EMS plant-level charge slots |
| `gateway_ems_discharge_slots` | HR 2040, 2044-2052 | Gateway / EMS plant-level discharge slots |

Slots 3-10 (on supported models) live in HR 240-299, with per-slot target SOCs interleaved. Three-phase models use HR 1080-1124 for slot/target registers (mirroring the single-phase layout at different addresses). Gateway / EMS plant-level scheduling uses HR 2040-2071 for charge and discharge slots. **GE Cloud UI** labels slots in opposite order — the data is identical, only labels differ. `ControlPage.tsx` shows yellow callout banners for: (a) the slot naming mismatch (any 2+ slot hybrid), (b) legacy Gen3 firmware (ARM FW ≤ 302) where extended HR 240-299 may return stale data.

### Discharge slot handling

Discharge Schedule is always visible regardless of mode. In Eco mode, edits are client-side only (no API call — prevents Gen3 firmware quirk where non-zero slot auto-enables discharge). Timed mode button is locked until at least one discharge slot is configured. Switching to Timed mode writes slots before `enable_discharge=1` flag (prevents unrestricted export). Switching from Timed to Eco clears all discharge slot registers.

### Optional block carry-forward

Multiple optional register blocks are conditionally polled, grouped by device type:

| Block group | Range | Used by |
|---|---|---|
| `EXTENDED_SLOTS_BLOCK` | HR 240-299 | Gen3, AIO, HV Gen3, AC-three-phase |
| `AC_CONFIG_BLOCK` | HR 300-359 | AC-coupled, AIO, AC-three-phase |
| `THREE_PHASE_HIGH_CONFIG_BLOCK` | HR 1000-1079 | Three-phase (real-time control, battery reserve) |
| `THREE_PHASE_CONFIG_BLOCK` | HR 1080-1124 | Three-phase (battery limits, charge/discharge slots) |
| `THREE_PHASE_INPUT_BLOCKS` (×7) | IR 1000-1413 | Three-phase (real-time telemetry) |
| `GATEWAY_INPUT_BLOCKS` (×5) | IR 1600-1859 | Gateway / EMS aggregation hub |

When an optional block read fails, `carry_forward_optional_block_values()` preserves previous values rather than flashing defaults/zeros in the UI for one cycle.

## Known issues

### Linux toolbar icon not showing (GNOME Wayland)

GNOME Wayland 43+ resolves the icon entirely through **application ID matching** (window GTK app ID must match a `.desktop` file ID). Fix: set `"enableGTKAppId": true` in `tauri.conf.json`. Dev mode workaround: run `bash scripts/install-dev-desktop.sh` once. Packaged `.deb`/`.rpm` installs handle this automatically.

### macOS minimum version: 10.15 (Catalina)

The app sets `bundle.macOS.minimumSystemVersion` to `"10.15"` in `tauri.conf.json`. This is because Vite's default build target emits modern JS syntax (optional chaining, nullish coalescing, etc.) that Safari 12 / WebKit on macOS 10.14 (Mojave) cannot parse, resulting in a blank white screen. Users on 10.14 or earlier will see a clear macOS dialog explaining the requirement instead.

Users on unsupported Macs can try [OpenCore Legacy Patcher](https://github.com/dortania/OpenCore-Legacy-Patcher) to install a newer macOS version.

### macOS 26.5 blocks ad-hoc signed binaries

macOS 26.5 blocks ad-hoc signed binaries inside `/Applications`. Three issues: (1) `/Applications` block — mitigated via one-time "Open Anyway" approval; (2) Gatekeeper on `open` — mitigated via `xattr -d com.apple.quarantine`; (3) x86_64 crashes under Rosetta — use aarch64 builds. The DMG workflow is standard; a `launch.command` script in the project root bypasses `/Applications` by searching Desktop first.

## Release process

1. Bump version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`
2. Update `CHANGELOG.md` with a new heading
3. Commit, then **immediately tag** (`vX.Y.Z`) — match the changelog heading exactly. Every version heading must have a corresponding git tag. Push both.
4. GitHub Actions builds for macOS (ARM + x64), Linux, Windows and creates a GitHub Release

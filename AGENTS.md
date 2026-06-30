# Home Energy Manager

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## General rules

- **Never close GitHub issues or PRs** without explicit permission from the project owner.
- **GitHub issue comments should read like a person wrote them.** That means no bullet-point recaps, no `**What changed**` / `**Verified**` / `**Why**` headings, no verification-checklist blocks at the end. Same goes for PR descriptions and review replies. Write the way you'd actually reply to a colleague in chat — acknowledge what they said, explain the substance, point at the fix. Technical detail is fine; changelog formatting is not.
- **Always add tests for any new behaviour.** Every bug fix and every new feature ships with test coverage. Frontend logic goes in `tests/lib/*.test.ts` (pure helpers) or `tests/pages/*.test.tsx` (component / page tests); Rust logic goes inline as `#[cfg(test)] mod tests` next to the code. Mirror the patterns already in those directories (the existing `tests/lib/deviceCapabilities.test.ts`, `tests/pages/settingsPageGridLines.test.tsx`, and the `poll.rs` `MAX_CONSECUTIVE_TIMEOUTS` test are good templates). If you're adding a new endpoint, a new state-machine branch, a new UI toggle, a new sanitization rule, or a new setting field, there must be a test that exercises it — including the failure / edge-case paths. "I'll add tests later" is not acceptable; tests are part of the change, not a follow-up.
- **Don't leave long-running processes behind.** Do not start `npm run dev`, `cargo tauri dev`, or other dev servers for testing unless explicitly asked — the project's test commands (`cargo test`, `npm run test:e2e`, `npm run build`) run to completion on their own. If you do start a long-running process for a legitimate reason, you must stop it yourself before finishing, and verify the port is freed. Never leave a process bound to a port (5173, 7337, etc.) after the task ends.

## Stack

- **Frontend**: React 19 + TypeScript + Vite 9 + Tailwind CSS 4 + Zustand + Recharts + React Router 7
- **Backend**: Tauri 2 desktop shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: Custom Rust TCP client to GivEnergy data adapter (port **8899**) aligned with [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) reference library and [GivTCP](https://github.com/dewet22/giv_tcp)
- **Testing**: Rust unit tests (inline `#[cfg(test)] mod tests` throughout `src-tauri/src/`) + integration tests with mock TCP server + Playwright end-to-end tests. Local-only E2E use [GivEnergy Simulator](https://github.com/psylsph/givenergy-simulator)
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
| `npm run lint:md` | Lint all markdown (`markdownlint`); run after significant .md edits |
| `npm run check:versions` | Verify `package.json` / `Cargo.toml` / `tauri.conf.json` agree |
| `npm run test` | Vitest + all `tests/scripts/*.test.sh` smoke tests (incl. `check-versions`) |
| `npm run preview` | `vite preview` (preview the production build locally) |
| `cargo test` (in `src-tauri/`) | Run all Rust unit tests |
| `cargo clippy` (in `src-tauri/`) | Run Rust linter |
| `cargo tauri dev` | Dev mode with Tauri window + Vite + hot-reload |
| `cargo tauri build` | Production build |
| `docker build .` | Docker container build |
| `npm run test:e2e` | Playwright E2E (requires `npm run build` + `cargo build --release` first) |
| `npm run test:local` | Local-only E2E using simulator at `~/repos/givenergy-simulator/target/release/sim-api` |
| `npm run test:local:headed` | Same as above with visible browser |

### Dongle misbehaviour tests

The simulator supports `--dongle-misbehaviour` to simulate various dongle failure modes:

| Mode | Behaviour | What it tests |
|---|---|---|
| `Off` | Normal operation | Baseline |
| `DropConnection` | Drops TCP immediately on accept | Hard error → reconnect |
| `Intermittent` | ~50% zeros, 50% real data | Per-block retry on timeout |
| `EmptyData` | All registers return 0 | Sanitizer zero detection |
| `StaleData` | Frozen register values (snapshot on first read) | Stale data detection |
| `GarbageData` | Random u16 values for every register | Sanitizer garbage rejection |

Tests in `e2e/local-dongle-misbehaviour.spec.ts` start their own simulator +
backend per misbehaviour mode, so they don't interfere with the main local
E2E suite. They run as part of `npm run test:local` (the `local-*.spec.ts`
glob in `playwright.local.config.ts` catches them).

Each test verifies that the backend's per-block retry and sanitization layers
handle the failure mode correctly:

- **DropConnection**: backend enters Reconnecting state, reconnects on retry
- **Intermittent**: backend stays Connected, snapshot eventually returns valid data
- **EmptyData**: backend stays Connected, snapshot shows zero power fields
- **StaleData**: backend stays Connected, snapshot values are frozen across polls

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

Run `npm run lint:md` after significant .md edits.

## Architecture

### Frontend (`src/`)

Entrypoint: `src/main.tsx`.

- **Pages**: `StatusPage`, `BatteryPage`, `HistoryPage`, `ControlPage` (model-aware rate scaling, slot-labelling warnings), `InverterPage`, `PowerPage`, `SolarPage`, `MetersPage`, `SettingsPage`, `LogsPage` (developer mode only)
- **Components**: `EnergyOrbitDiagram` (radial SVG power flow; renamed from `EnergyFlowDiagram`), `BatteryPanel`, `BatteryGauge`, `SummaryTiles`, `SolarPowerChart`, `BatterySocChart`, `SeriesLegend`, `ColdBatteryWarning`
- **Hooks**: `useWebSocket` — connects to `/ws`, reconnects on drop, fetches snapshot via REST
- **Lib**: `api.ts` (fetch helpers), `format.ts` (formatters), `types.ts` (types), `evcLabel.ts` (EV Charger state → label picker: `Charging` / `Connected` / `Disconnected` / `Not Found`), `validators.ts` (`isValidIpv4Host` for the EVC Charger Address field)
- **State**: Zustand store (`useInverterStore`) — snapshot, connectionState, connectedHost, developerMode (persisted to localStorage), EV Charger state (`evcHost`, `evcPower`, `evcCharging`, `evcConnected`, `evcEverConnected` latch). The latch distinguishes "charger was here, now offline" from "we've never successfully reached the host" so the diagram can show "Disconnected" vs "Not Found" (issue #138). `resetEvc()` clears the latch when the user saves a new host.
- **Version**: `__APP_VERSION__` (from `vite.config.ts`)

Frontend talks exclusively to the local Axum server — never directly to the inverter.

### Backend (`src-tauri/src/`)

- **`lib.rs`** — Tauri app setup + headless CLI; spawns Axum server + Modbus polling loop. Two independent tracing layers: `fmt` layer to stdout (level WARN, override via `RUST_LOG`) and `LogCaptureLayer` into in-memory `LogRing` for dev console (level WARN, runtime-adjustable via `PUT /api/log-level`).
- **`history/`** — SQLite-backed history (`~/.givenergy-local/history.db`). `HistoryDb` wrapper, schema migration, `insert_reading()`, aggregated `query_history()` with time-bucket AVG (or MAX for cumulative fields).
- **`inverter/`** — data model, register decode/encode, discovery, poll loop, sanitization
  - `model.rs` — `InverterSnapshot`, `ScheduleSlot`, `BatteryMode`, `BatteryState`, `DeviceType` enum (Gen1-4, AC-coupled, three-phase, AIO, HV Gen3/4, EMS) with model-aware helpers for slave addresses, poll blocks, slot counts, battery protocol selection.
  - `decoder.rs` — converts raw register blocks into `InverterSnapshot`; per-block decoders for holding registers 0-59, 60-119, 240-299 (extended 10-slot schedules), 300-359 (AC config), 1080-1124 (three-phase config).
  - `encoder.rs` — translates `ControlCommand` into whitelist-validated `RegisterWrite` lists. Model-specific commands for AC-coupled and three-phase limits.
  - `sanitizer.rs` — the register-corruption defense layer (absolute range checks, delta/rate checks, grace-period median-of-3 hardening). Formerly inline in `poll.rs`; now its own module — the "Data sanitization" section below documents its rules in detail.
  - `poll.rs` — main polling loop: drain writes → read registers → sanitize (via `sanitizer.rs`) → broadcast snapshot. Features: dongle memory-leak fingerprint detection, model-aware slave address switching, carry-forward for optional blocks, two battery protocols (LV at 0x32+; HV at 0xA0→0x70+/0x50+), derived three-phase battery fields.
  - `discovery.rs` — network scanning with GivEnergy Modbus protocol verification (validates 0x5959 magic header).
  - `state_machines.rs` — connect/reconnect and battery-protocol state machines.
- **`modbus/`** — GivEnergy Modbus TCP protocol
  - `client.rs` — `ModbusClient`: connect, read registers, write single register (FC6), stale frame drain, heartbeat handling (echoes dongle heartbeats). Default slave address `0x11`. `read_all_with_extras()` decides optional blocks by device type.
  - `framer.rs` — proprietary frame encode/decode (MBAP header + transparent sub-frame + CRC)
  - `registers.rs` — register addresses, poll block definitions, safe-write whitelist, HHMM encode/decode. Standard blocks: `IR(0,60)`, `HR(0,60)`, `HR(60,60)`, `IR(180,4)` (alternative battery lifetime/daily totals for Gen1 Hybrid). Per-battery BMS reads (`BATTERY_1_POLL_BLOCK`, `BATTERY_POLL_BLOCK` = `IR(60,60)`) and HV stack reads (`HV_BCU_POLL_BLOCK` = `IR(60,60)` at device `0x70+`, BMU blocks at `0x50+`) are polled separately, not part of `STANDARD_POLL_BLOCKS`. Optional blocks (conditionally polled by device type): `EXTENDED_SLOTS_BLOCK` (HR 240-299), `AC_CONFIG_BLOCK` (HR 300-359), `THREE_PHASE_HIGH_CONFIG_BLOCK` (HR 1000-1079), `THREE_PHASE_CONFIG_BLOCK` (HR 1080-1124), seven `THREE_PHASE_INPUT_BLOCKS` (IR 1000-1413), five `GATEWAY_INPUT_BLOCKS` (IR 1600-1859).
- **`server/`** — Axum HTTP layer: `api.rs` (REST endpoints), `ws.rs` (WebSocket snapshot stream), `logs.rs` (LogRing + `GET /api/logs`), `mod.rs` (router + graceful bind). EVC endpoints: `GET /api/evc/discover` (network scan), `GET /api/evc/status` (current reachability + cached snapshot, used by the frontend on page load to seed `evcEverConnected` before the next WS broadcast).
- **`evc/`** — EV Charger (OCPP/Modbus) client + poll loop. Standard Modbus TCP on port 502; broadcasts `PollMessage::Evc` on every successful snapshot, `PollMessage::EvcConnected` immediately on TCP handshake (before first register read), and `PollMessage::EvcDisconnected` on invalid-host parse error or connect failure. Invalid-host parsing also clears `latest_evc` so the frontend's `/api/evc/status` reports the right `reachable=false`.
- **`settings/`** — persisted JSON config (`~/.givenergy-local/settings.json`)
- **`alerts/`** — alert evaluation engine + push notifications. Evaluates each sanitized `InverterSnapshot` against user thresholds (battery temperature high/low, battery SOC high/low, solar-clipping ceiling, inverter battery-warning flag, grid offline) with per-type cooldown and consecutive-read confirmation, then delivers via the **Telegram Bot API** and/or **ntfy.sh** (including self-hosted ntfy). Also generates/sends the daily consumption report and polls Telegram for `/status`, `/today`, `/report` commands. **This covers GitHub issue #85 (critical-condition notifications) — implemented as Telegram + ntfy push notifications rather than email.**

### Shared state (`AppState`)

`Arc<Mutex<…>>` shared between poll loop, API handlers, and WebSocket: `latest_snapshot`, `connection_state`, `pending_writes`, `write_notify` (wakes poll loop), `settings`, `history`, `log_ring` (2000-entry ring buffer).

## Data sanitization (register corruption defense)

GivEnergy dongle frequently returns corrupted register values. The sanitizer (in `inverter/sanitizer.rs`; formerly inline in `poll.rs`) defends with multiple layers:

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
Connect → 500ms delay → drain TCP → 1× warmup read (discarded)
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

Slots 3-10 (on supported models) live in HR 240-299, with per-slot target SOCs interleaved. Three-phase models use HR 1080-1124 for slot/target registers (mirroring the single-phase layout at different addresses). EMS / EmsCommercial plant-level scheduling uses HR 2040-2071 for charge and discharge slots. **The Gateway is single-phase-class for control** (issue #149): its Quick Actions / charge & discharge schedule write to the standard HR 94/95, 56/57, 96, 116 registers (forwarded to its child AIOs), NOT the three-phase HR 1080-1124 bank (which a real Gateway dongle has no registers for) nor the EMS HR 2040-2071 schedule. The Gateway *does* poll HR 2040-2075 for plant-level config (export limit, plant enable) read-back. **GE Cloud UI** labels slots in opposite order — the data is identical, only labels differ. `ControlPage.tsx` shows yellow callout banners for: (a) the slot naming mismatch (any 2+ slot hybrid), (b) legacy Gen3 firmware (ARM FW ≤ 302) where extended HR 240-299 may return stale data.

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

GNOME Wayland 43+ resolves the icon entirely through **application ID matching** (window GTK app ID must match a `.desktop` file ID). Fix: set `"enableGTKAppId": true` in `tauri.conf.json`. For dev mode, `npm run dev:desktop` (wired into Tauri's `beforeDevCommand`) writes / refreshes `~/.local/share/applications/com.givenergy.local.desktop` on every `cargo tauri dev` so the dock icon can't go stale if the repo moves. Packaged `.deb`/`.rpm` installs handle this automatically via their own .desktop file.

### macOS minimum version: 10.15 (Catalina)

The app sets `bundle.macOS.minimumSystemVersion` to `"10.15"` in `tauri.conf.json`. This is because Vite's default build target emits modern JS syntax (optional chaining, nullish coalescing, etc.) that Safari 12 / WebKit on macOS 10.14 (Mojave) cannot parse, resulting in a blank white screen. Users on 10.14 or earlier will see a clear macOS dialog explaining the requirement instead.

Users on unsupported Macs can try [OpenCore Legacy Patcher](https://github.com/dortania/OpenCore-Legacy-Patcher) to install a newer macOS version.

### macOS 26.5 blocks ad-hoc signed binaries

macOS 26.5 blocks ad-hoc signed binaries inside `/Applications`. Three issues: (1) `/Applications` block — mitigated via one-time "Open Anyway" approval; (2) Gatekeeper on `open` — mitigated via `xattr -d com.apple.quarantine`; (3) x86_64 crashes under Rosetta — use aarch64 builds. The DMG workflow is standard; a `launch.command` script in the project root bypasses `/Applications` by searching Desktop first.

## Release process

1. Bump version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`. `npm run check:versions` (also a step in CI and the first check in `npm test`) fails if the three drift — this is the guard against the "bumped two, forgot one" mistake that shipped v0.33.2 with `tauri.conf.json` still reading `0.33.1`.
2. Update `CHANGELOG.md` with a new heading
3. Commit, then **immediately tag** (`vX.Y.Z`) — match the changelog heading exactly. Every version heading must have a corresponding git tag. Push both. The release build (`.github/workflows/build.yml`, triggered by `v*` tags) runs `check-versions` as a gating job before any platform build starts, so an out-of-sync tag fails fast instead of producing installers whose bundled version disagrees with the release name.
4. GitHub Actions builds for macOS (ARM + x64), Linux, Windows and creates a GitHub Release

### Changelog style

The changelog is for users, not developers. Each entry should be a short
bullet that leads with a bold one-line summary and adds one or two
sentences of substance — what the user will notice, what they can now
do, what stops being broken. Avoid exhaustive technical detail: no
register numbers, no algorithm names, no `**What changed**` /
`**Verified**` /`**Why**` headings, no "Files touched" lists. Reference
issue/PR numbers only when the entry closes a specific user-reported
issue. The existing entries in `CHANGELOG.md` are the canonical
examples of the voice and length to match.

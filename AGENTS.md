# Home Energy Manager

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## Stack

- **Frontend**: React 19 + TypeScript + Vite 9 + Tailwind CSS 4 + Zustand + Recharts + React Router 7
- **Backend**: Tauri 2 desktop shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: Custom Rust TCP client to GivEnergy data adapter (port **8899**) aligned with [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) reference library and [GivTCP](https://github.com/dewet22/giv_tcp)
- **Testing**: Rust unit tests (241) + integration tests with a mock TCP server that simulates GivEnergy dongle behaviour (no frontend tests)
- **References**: Local clones at `~/repos/givenergy-modbus` and `~/repos/giv_tcp` are the source of truth for register layout, slot maps, slave addressing, and command encoding

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
| `cargo test` (in `src-tauri/`) | Run all Rust unit tests (241 tests) |
| `cargo clippy` (in `src-tauri/`) | Run Rust linter |
| `cargo tauri dev` | Dev mode with Tauri window + Vite + hot-reload |
| `cargo tauri build` | Production build of the desktop app |

Order for full verification: `cargo clippy` → `npm run lint` → `npm run lint:md` → `npm run build` (typechecks) → `cargo test` in `src-tauri/`.

## Linting rules

### Rust (clippy)

All clippy warnings must be fixed before committing. Known patterns that commonly trigger warnings:

- `empty_line_after_doc_comments` — no blank line after `///` doc comments
- `field_reassign_with_default` — use `Struct { field: value, ..Default::default() }` instead of mutating after default
- `manual_flatten` — use `.flatten()` / `.into_iter().flatten()` instead of `if let Some` in a loop
- `match_like_matches_macro` — use `matches!()` for boolean match expressions
- `derivable_impls` — use `#[derive(Default)]` instead of manual `impl Default`
- `new_without_default` — add `impl Default` that calls `new()`
- `same_item_push` — use `vec![val; N]` or `resize(N, val)` instead of loop + push
- `manual_clamp` — use `.clamp(min, max)` instead of `.min(max).max(min)`

Run: `cd src-tauri && cargo clippy`

### TypeScript / ESLint

All ESLint errors must be fixed before committing. Notable rules:

- `verbatimModuleSyntax: true` — use `import type` for type-only imports
- `erasableSyntaxOnly: true` — no `enum`, no `namespace`, no constructor parameter properties
- `noUnusedLocals` / `noUnusedParameters` — both on; declarations must be used
- `react-hooks/set-state-in-effect` — do not call `setState` directly inside `useEffect`; use derived values or key-based remounting instead

Run: `npm run lint`

### Markdown

Run `markdownlint` on .md files after significant edits. Notable rules:

- MD001 — heading levels should only increment by one level at a time
- MD012 — no multiple consecutive blank lines
- MD022 — headings should be surrounded by blank lines
- MD032 — lists should be surrounded by blank lines

Run: `npx markdownlint '**/*.md' --ignore node_modules`

## Architecture

### Frontend (`src/`)

React app. Entrypoint: `src/main.tsx`.

- **Pages**: `StatusPage` (dashboard + energy flow), `BatteryPage` (cell-level detail), `HistoryPage` (charts), `ControlPage` (schedules, modes, limits — model-aware rate scaling and slot-labelling warnings), `InverterPage` (device info: serial, ARM + DSP firmware versions, device type, rated powers), `SettingsPage` (connection config, connected clients, developer mode, about), `LogsPage` (developer console — only visible when developer mode is enabled)
- **Components**: `EnergyFlowDiagram` (radial SVG power flow), `BatteryPanel` (per-module cell data), `SummaryTiles` (power stats)
- **Hooks**: `useWebSocket` — connects to `/ws`, reconnects on drop, fetches initial snapshot via REST
- **Lib**: `api.ts` (fetch helpers), `format.ts` (power/voltage/temp formatters), `types.ts` (InverterSnapshot etc.)
- **State**: Zustand store (`useInverterStore`) holds `snapshot`, `connectionState`, `connectedHost`, `developerMode` (persisted to localStorage)
- **Version**: Injected at build time via `__APP_VERSION__` (defined in `vite.config.ts`, declared in `src/env.d.ts`)

Frontend talks exclusively to the local Axum server — never directly to the inverter.

### Backend (`src-tauri/src/`)

- **`lib.rs`** — Tauri app setup + headless CLI entry; spawns Axum server (configurable port, default 7337) + Modbus polling loop. Sets up tracing with **two independent layers**: a `fmt` layer to stdout/stderr (default level **WARN**, override with `RUST_LOG`) and a `LogCaptureLayer` into the in-memory `LogRing` for the developer console (default capture level WARN, runtime-adjustable via `PUT /api/log-level`). The two layers filter independently — changing the console default does not affect the developer console and vice versa.
- **`history/`** — SQLite-backed history storage (`~/.givenergy-local/history.db`)
  - `mod.rs` — `HistoryDb` wrapper, schema migration, `insert_reading()`, aggregated `query_history()` with time-bucket AVG (or MAX for cumulative fields)
- **`inverter/`** — data model, register decode/encode, discovery, poll loop
  - `model.rs` — `InverterSnapshot`, `ScheduleSlot`, `BatteryMode`, `BatteryState`, plus `DeviceType` enum (Gen1/Gen2/Gen3 hybrids, AC-coupled variants, three-phase/commercial, AIO, HV Gen3, Gen4, EMS) with model-aware helpers: `preferred_read_slave_address()`, `supports_gen3_extended()`, `extra_poll_blocks()`, `max_charge_slots()`, `max_discharge_slots()`, `max_battery_power_for_dtc()`, `uses_hv_battery()` (gates the HV BCU/BMU battery path vs the LV 0x32 path). Gen3 generation is resolved from `arm_fw / 100` (3 → Gen3, 8/9 → Gen2, else Gen1).
  - `decoder.rs` — converts raw register blocks into `InverterSnapshot`; applies global `enable_charge`/`enable_discharge` flags to slot states; per-block decoders for `holding_0_59`, `holding_60_119`, `holding_240_299` (extended 10-slot schedules), `holding_300_359` (AC-coupled config: HR313/314 limits, HR311 export priority, HR317 EPS, HR318-320 pause slot), `holding_1080_1124` (three-phase config: HR1108 discharge limit, HR1109 SOC reserve, HR1110 charge limit, HR1111 target SOC, HR1112/1122/1123 force/AC-charge flags). AC-coupled models skip HR111/112; three-phase models read limits from HR1110/1108 instead.
  - `encoder.rs` — translates `ControlCommand` enum into `RegisterWrite` lists (whitelist-validated). Includes model-specific commands: `SetAcChargeLimit`/`SetAcDischargeLimit` (HR313/314, 1-100%), `SetThreePhaseChargeLimit`/`SetThreePhaseDischargeLimit` (HR1110/1108, 1-100%), `SetThreePhaseBatterySocReserve` (HR1109), `SetThreePhaseChargeTargetSoc` (HR1111). Standard DC hybrid `SetChargeLimit`/`SetDischargeLimit` use HR111/112 (0-50% register, displayed as 0-100%).
  - `poll.rs` — main polling loop: drain pending writes → read registers → sanitize → broadcast snapshot; uses `Notify` for immediate write execution; warmup reads and grace period after connect. Includes: (a) `is_suspicious()` dongle memory-leak fingerprint detection that flags 60-register blocks matching the known 7-fingerprint corruption pattern (matches givenergy-modbus `>5` threshold); (b) model-aware slave address switching — after first detection, switches to `preferred_read_slave_address()` for operational reads (0x31 for AC/Gen1, 0x11 for all others); (c) immediate re-poll after model detection when slave address changes or extra blocks are needed (`should_repoll_after_model_detection()`); (d) carry-forward for optional blocks (AC config HR300-359, extended slots HR240-299, three-phase config HR1080-1124) — if an optional block is missed on one poll, preserves previous values rather than flashing zeros in the UI; (e) **two battery protocols selected by `uses_hv_battery()`** — LV packs read BMS at slave 0x32 (+0x33-0x37); HV stacks (three-phase / AIO / HV Gen3 / Gen4) probe the BMS at 0xA0 once to discover BCU count, then read each BCU cluster (0x70+i, IR 60-119) every cycle for pack-level voltage/current/temperature/capacity plus each BMU (0x50+m, IR 60-119 base-shifted by 120·bcu_offset) for per-module cell detail; (f) median-of-3 grace-period baseline hardening (`GraceCumulativeSamples`) — the 3 grace-period readings of every cumulative counter are collected and, on the final grace reading, replaced with the per-field median, so a single corrupted-but-in-range grace value can't poison the delta-check baseline for every subsequent reading; (g) `derive_three_phase_battery_fields()` — three-phase/HV/AIO inverters have no battery temperature or capacity in their inverter register blocks (only converter heatsink temps), so this derives `battery_temperature` / `battery_capacity_kwh` / `max_battery_power_w` from the HV BCU cluster (authoritative pack voltage/current/temperature/capacity) when present, else falls back to the LV BMS module data after the 0x32 read completes. No-op for single-phase (which gets those values directly from IR(56)/HR(55)). When no BMS data is available, clears any garbage and falls back to the uncapped hardware power limit.
  - `discovery.rs` — network scanning with GivEnergy Modbus protocol verification (sends a read request and validates the 0x5959 magic header in the response)
- **`modbus/`** — GivEnergy Modbus TCP protocol
  - `client.rs` — `ModbusClient`: connect, read registers, write single register (FC6), stale frame drain, heartbeat handling. A background consumer task owns the read half of the split TCP stream and routes incoming frames to pending futures by content key (slave+function+register range); it also **echoes dongle heartbeat requests** (function 0x01) back to the dongle via the shared `Arc<Mutex<OwnedWriteHalf>>` — without the echo the dongle closes the socket after 3 missed heartbeats (~9 min). Writes (request frames + heartbeat responses) are serialised through the same mutex. **Default slave address is `0x11`** (canonical detection address per givenergy-modbus and GivTCP), not `0x32`. `read_all_with_extras()` takes `device_type` and `arm_fw` to decide which optional blocks (extended schedules, AC config, three-phase config) to poll.
  - `framer.rs` — proprietary frame encode/decode (MBAP header + transparent sub-frame + CRC); response CRC validation is lenient (logged, not rejected)
  - `registers.rs` — register addresses, poll block definitions, safe-write whitelist, HHMM encode/decode. Standard poll blocks: `IR(0,60)`, `HR(0,60)`, `HR(60,60)`, plus per-battery `IR(60,60)` blocks. Optional model-specific blocks: `EXTENDED_SLOTS_BLOCK` (HR240-299), `AC_CONFIG_BLOCK` (HR300-359), `THREE_PHASE_CONFIG_BLOCK` (HR1080-1124), plus composite constants `AC_AND_THREE_PHASE_BLOCKS`, `EXTENDED_AND_THREE_PHASE_BLOCKS`. HV battery addresses: `HV_BMS_ADDRESS` (0xA0, IR61 = BCU count), `HV_BCU_BASE_ADDRESS` (0x70, cluster read IR 60-119), `HV_BMU_BASE_ADDRESS` (0x50, per-module cell read). `SAFE_WRITE_REGS` is the union of the givenergy-modbus safe-write allowlist and is asserted against key addresses in unit tests.
- **`server/`** — Axum HTTP layer
  - `api.rs` — REST endpoints for control commands; queues writes to `AppState::pending_writes` and notifies poll loop
  - `ws.rs` — WebSocket endpoint streaming `PollMessage` (snapshot or connection state)
  - `logs.rs` — Log ring buffer (`LogRing`) + tracing capture layer + `GET /api/logs` endpoint for developer console
  - `mod.rs` — router setup, server startup (graceful bind failure, no panic)
- **`settings/`** — persisted JSON config (`~/.givenergy-local/settings.json`)

### Shared state (`AppState`)

Central `Arc<Mutex<…>>`-based state shared between poll loop, API handlers, and WebSocket:

- `latest_snapshot` — most recent `InverterSnapshot`
- `connection_state` — `Connected` / `Disconnected`
- `pending_writes` — queue of `Vec<RegisterWrite>` batches from the API
- `write_notify` — `Notify` that wakes the poll loop immediately when writes are queued
- `settings` — live `PollSettings` (host, port, serial, interval)
- `history` — `HistoryDb` for time-series storage
- `log_ring` — `LogRing` (2000-entry ring buffer) of captured log lines for the developer console

## Data sanitization (register corruption defense)

The GivEnergy dongle frequently returns corrupted register values, especially
on the first reads after TCP connect. The sanitizer in `poll.rs` defends against
this with multiple layers:

### Absolute range checks (always active)

Applied on EVERY reading regardless of previous state:

| Field | Range | Notes |
|---|---|---|
| `today_*_kwh` | 0–200 kWh | Residential daily ceiling; catches 245, 275, 879, 1010 spikes |
| Battery power | ±10 kW | Residential battery limit |
| Grid power | ±10 kW | UK single-phase supply limit |
| Solar power | 0–10 kW | Residential PV limit |
| Home power | 0–15 kW | Includes EV charging margin |
| Grid voltage | 180–280 V | UK nominal 230V ± extended range |
| Grid frequency | 45–55 Hz | UK nominal 50 Hz |
| Inverter temp | -20–100 °C | Hardware damage above 100°C |
| Battery temp | -20–80 °C | Safety limit |
| Battery module voltage | 0–500 V | LV (~48V) to HV (~345V) |
| SOC | 0–100 | Also rejects SOC=0 with live power, SOC=100 while fast-charging |

### Delta checks (after grace period)

Only active after 3 readings post-connect (grace period):

- **Monotonic increase**: `today_*_kwh` must not decrease significantly (except midnight rollover)
- **Time-based rate limit**: `max_increase = elapsed_hours × 10 kW + 1 kWh`
- **Jitter tolerance**: decreases < **0.15 kWh** accepted as normal dongle register precision noise (carried forward silently, no re-poll)
- **Midnight rollover**: decrease allowed when `raw < 5` and `prev > 5`
- **Near-zero prev**: when `prev < 1.0`, the baseline is unreliable — instead of skipping the check entirely, a **tighter time-aware ceiling** (`max_increase_kwh`) is applied to catch plausibly corrupted near-zero values (the old skip-open approach let corrupted values like 42.5 through after prev was clamped to 0)

### Connect sequence

```
Connect → 500ms delay → drain TCP → 3× warmup reads (discarded, 500ms apart)
→ clear latest_snapshot → 3 readings with absolute check only (grace period)
  └─ cumulative counters from all 3 grace readings collected
  └─ on 3rd grace reading: replaced with per-field MEDIAN of the 3 samples
     (so a single corrupted spike can't poison the delta baseline)
→ readings with full absolute + delta checks
```

### History aggregation

The history API (`GET /api/history`) uses MAX aggregation for cumulative
counter fields (`today_*_kwh`) instead of AVG. AVG of monotonically increasing
counters understates the true value, causing ~1000× cost inflation when deltas
are computed. MAX preserves the actual counter reading at each bucket boundary.

### Frontend spike filtering

`removeSpikes()` in `HistoryPage.tsx` applies a post-query filter: a point is
a spike if it differs from both neighbors by more than a field-specific
threshold while the neighbors differ by less than half the threshold.

## Modbus write protocol

Per the [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) reference library:

- **Function code 6** (Write Single Holding Register) — one register per request
- **Default device address `0x11`** — canonical detection address used by both givenergy-modbus and GivTCP. The ModbusClient defaults to `0x11` and switches to the model-specific operational address after detection (`0x31` for AC-coupled and Gen1 Hybrid; `0x11` for everything else). Battery BMS reads use the model-appropriate protocol: LV packs at `0x32`/`0x33+`, HV stacks at `0x70+` (BCU cluster) / `0x50+` (BMU per-module), discovered via the BMS at `0xA0`.
- **CRC/check**: `CrcModbus(function_code + register + value)` — computed per the reference library
- **Slot clearing**: write `0` (not sentinel 60) — `00:00–00:00` is treated as disabled
- **Retry policy**: 6 attempts with 2s delay for exception code 67 (dongle busy); fail fast and continue

### Model-specific write targets

The same UI control may write different registers depending on the inverter model:

| UI control | DC hybrid (Gen2/Gen3+) | AC-coupled | Three-phase / commercial / HV |
|---|---|---|---|
| Charge power limit | HR111 (0-50%) | HR313 (1-100%) | HR1110 (1-100%) |
| Discharge power limit | HR112 (0-50%) | HR314 (1-100%) | HR1108 (1-100%) |
| Battery SOC reserve | HR110 | HR110 | HR1109 |
| Charge target SOC | HR116 | HR116 | HR1111 |

The API routes (`set_charge_rate`, `set_discharge_rate`, `set_reserve` in `server/api.rs`) inspect the latest snapshot's `device_type` to choose the right command. The frontend (`ControlPage.tsx`) similarly picks the correct register max (50 vs 100) and display formula based on the device type code.

Known limitation: register 32 (charge slot 2 end time) consistently returns exception 67 on some inverters despite being in the reference library's safe-write list. The system handles this gracefully — `enable_charge` flag still updates correctly.

## TypeScript quirks

- `verbatimModuleSyntax: true` — use `import type` for type-only imports
- `erasableSyntaxOnly: true` — no `enum`, no `namespace`, no `constructor parameter properties`
- `noUnusedLocals` / `noUnusedParameters` — both on; declarations must be used
- ESLint rule `react-hooks/set-state-in-effect` — do not call `setState` directly inside `useEffect`; use key-based remounting or derived values instead

## Rust testing

All Rust tests are `#[cfg(test)]` unit tests or integration tests with a mock TCP server that simulates GivEnergy dongle behaviour. Run with:

```
cd src-tauri && cargo test
```

(The mock TCP server is in `modbus/client.rs` and is also used by the e2e Playwright test suite for full-stack scenarios.)

## Build artifacts

- `dist/` — Vite output (frontend)
- `src-tauri/target/` — Rust build output
- `node_modules/.tmp/tsconfig.*.tsbuildinfo` — TypeScript incremental build info

## Headless server mode (Linux)

Run without a Tauri window — just the Axum HTTP/WS server and Modbus poll loop. Ideal for Raspberry Pi or always-on servers.

```bash
# Build the frontend first
npm run build

# Build the binary
cd src-tauri && cargo build --release

# Run headless
./target/release/givenergy-local --headless
./target/release/givenergy-local --headless --port 8080
./target/release/givenergy-local --headless --dist /path/to/dist
```

The `--dist` flag specifies the frontend static files directory. Search order: `--dist` arg > `./dist/` (cwd) > `<exe_dir>/dist/` > `/usr/share/givenergy-local/dist/`. If no dist is found, runs API-only (REST + WebSocket still work).

## Schedule slot register layout (and the GE Cloud label mismatch)

Schedule slot registers are documented in `model/slot_map.py` of the reference library. There are four physical slot register pairs in the standard poll blocks:

| Reference library name | Register | UI label |
|---|---|---|
| `charge_slot_1` | HR 94-95 | Slot 1 (per canonical naming) |
| `charge_slot_2` | HR 31-32 | Slot 2 (per canonical naming) |
| `discharge_slot_1` | HR 56-57 | Slot 1 (per canonical naming) |
| `discharge_slot_2` | HR 44-45 | Slot 2 (per canonical naming) |

Slots 3-10 (on models that support them) live in the HR 240-299 extended block, with per-slot target SOCs interleaved (HR 242/245 for slots 1/2, then HR 248, 251, 254, …, 269 for slots 3-10).

### GE Cloud UI disagreement

GivEnergy's cloud portal appears to label the slots in the opposite order to both reference libraries (Cloud "Slot 1" = HR 31-32 / 44-45 = our "Slot 2"). This causes confusion for users coming from the cloud UI to our app, or vice-versa. The underlying data is identical — only the labels differ.

See [`issue #41`](https://github.com/psylsph/home-energy-manager/issues/41) for the original report and discussion.

### Frontend warnings

`ControlPage.tsx` shows yellow callout banners above Slot 2 in both Charge and Discharge Schedule sections to flag this:

1. **Slot ordering mismatch** — shown for any hybrid inverter with `max_charge_slots >= 2`. Explains the canonical-vs-cloud naming difference and links to issue #41.
2. **Legacy Gen3 firmware (ARM FW ≤ 302)** — shown only when `device_type_code` starts with `20` AND `firmware_version` (as integer) is `1..=302`. Warns that the extended HR 240-299 block may return stale or garbage data on this firmware, since the reference library and GivTCP only enable extended polling when `arm_fw > 302`.

The labelling warning fires for the issue #41 user's hardware (ARM FW 318). The legacy-firmware warning fires only for older Gen3 firmware (≤ 302) and is shown in addition to the labelling warning when both conditions are true.

## Optional block carry-forward

Three optional register blocks are conditionally polled based on device type:

- `EXTENDED_SLOTS_BLOCK` (HR 240-299) — Gen3 (ARM FW > 302), AIO, HV Gen3, Gen4, AllInOneHybrid
- `AC_CONFIG_BLOCK` (HR 300-359) — AC-coupled models and AC three-phase
- `THREE_PHASE_CONFIG_BLOCK` (HR 1080-1124) — three-phase, AC three-phase, AIO commercial, HV Gen3, AllInOneHybrid

When an optional block is requested but the read fails (timeout, exception, or skipped due to corruption), `carry_forward_optional_block_values()` in `poll.rs` preserves the previous snapshot's values for fields that only come from that block — instead of leaving them at default/zero. This prevents the UI from flashing misleading zeros for one poll cycle.

Flags passed in: `has_ac_config_block`, `has_extended_slots_block`, `has_three_phase_config_block` (computed from the actual `BlockRead`s returned in the current cycle, not from `device_type`). Carry-forward only triggers when the device type matches the expected model AND the optional block is absent in the current cycle.

## Known issues

### Linux toolbar icon not showing (GNOME Wayland)

**Symptom**: The app icon shows as a generic gear in the dock/taskbar on
GNOME Wayland. `set_icon()` in Rust succeeds (returns Ok) but the desktop
environment ignores the embedded window icon.

**Root cause**: GNOME Wayland 43+ deliberately ignores window icons set via
`gtk_window_set_icon()` / `Window::set_icon()`. It resolves the icon
entirely through **application ID matching**:

1. The window's GTK application ID (`app_id` on Wayland) must match a
   `.desktop` file ID (the filename without `.desktop`).
2. The icon is then taken from the `Icon=` key in that `.desktop` file.

**Configuration required in `tauri.conf.json`**:

```json
"app": {
  "enableGTKAppId": true
}
```

This sets the GTK app ID to the Tauri identifier (e.g. `com.givenergy.local`),
which becomes the surface `app_id` on Wayland.

**Dev mode workaround** (run once after `git pull` or fresh clone):

```bash
bash scripts/install-dev-desktop.sh
```

This creates `~/.local/share/applications/com.givenergy.local.desktop`
with the correct `app_id`-matching filename and the icon path set to
`src-tauri/icons/128x128.png`. The desktop entry is the canonical (non-hidden)
type so the compositor picks it up correctly.

**Packaged app** (installed via `.deb` / `.rpm`): the package already
installs a matching desktop file. The `postinst` script refreshes the
desktop database and icon cache automatically.

**Troubleshooting**: If the icon still doesn't appear after installing the
desktop file, check these steps:

1. Verify the file exists: `ls -la ~/.local/share/applications/com.givenergy.local.desktop`
2. Confirm `app.enableGTKAppId` is `true` in `src-tauri/tauri.conf.json`
3. Rebuild: `cargo tauri dev` (a full rebuild may be needed)
4. Check logs for `Window icon set successfully` — if that appears, the
   issue is purely at the DE/compositor level

## macOS 26.5 blocks ad-hoc signed binaries

**Symptom**: The app binary silently exits with no output and no port 7337 when
the .app bundle is installed in `/Applications`. Same binary runs fine from
Desktop, `/tmp`, or any user-level directory.

**Root cause**: macOS 26.5 (Sequoia) now blocks ad-hoc signed binaries
(`signingIdentity: "-"` in tauri.conf.json) from running inside the system
`/Applications` directory. This is stricter than previous macOS versions —
even running the binary directly via terminal fails, not just `open`.

**Three separate issues found on macOS 26.5**:

| Issue | Trigger | Status |
|---|---|---|
| 1. `/Applications` block | Binary launched from `/Applications` | **Mitigated** (one-time "Open Anyway" approval via System Settings) |
| 2. Gatekeeper blocks `open` | `open GivEnergy-Local.app` or double-click | **Mitigated** (one-time approval or `xattr -d com.apple.quarantine`) |
| 3. x86_64 binary crashes under Rosetta | macOS 26.5 + Rosetta | **Fixed** (documented — use aarch64 builds) |

**Standard DMG workflow**:
The DMG retains the standard `/Applications` symlink so drag-to-Applications
works as expected. On first launch, macOS 26.5 will show a Gatekeeper warning.
The user can approve it via:

1. `xattr -d com.apple.quarantine /Applications/Home\ Energy\ Manager.app`
2. System Settings → Privacy & Security → click "Open Anyway" next to the app
3. Or right-click the app → Open → Open

After this one-time approval the app runs normally from `/Applications`.

**launch.command**:
There is a `launch.command` script in the project root that searches Desktop
first (avoids the /Applications block entirely), then falls back to /Applications.
This is useful for headless/terminal use.

**Known good archs**:

- The aarch64 (ARM64) app works correctly from Desktop and from /Applications
  after one-time Gatekeeper approval
- The x86_64 (Intel) app crashes silently under Rosetta on macOS 26.5+
- Always use the aarch64 release on Apple Silicon Macs

## Release process

1. Bump version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`
2. Update `CHANGELOG.md` with a new heading for the version
3. Commit, then **immediately tag** (`vX.Y.Z`) — match the heading in the changelog
   exactly. Every version heading in CHANGELOG.md MUST have a corresponding git
   tag. No exceptions. Push both the commit and the tag.
4. GitHub Actions workflow (`.github/workflows/build.yml`) builds for macOS
   (ARM + x64), Linux, Windows and creates a GitHub Release with binaries

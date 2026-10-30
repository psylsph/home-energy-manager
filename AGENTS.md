# Home Energy Manager

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## General rules

**The project owner is Stuart** (`psylsph` on GitHub). Comments and replies posted on Stuart's behalf speak as Stuart — never sign or address him by any other name.

### Git and GitHub

- **Never commit, push, tag, or release without explicit permission.** Make the change, verify, report back, then wait for Stuart to say "commit" or "push". Each approval covers one commit or push only.
- **Commit directly on `master`** when permitted — single developer, so no feature branches or worktrees. Dependabot bumps and outside contributions still arrive as PRs; merge them into `master`.
- **No `Co-Authored-By: Claude` (or similar AI attribution) trailers** on commits or PRs.
- **Never close GitHub issues or PRs** without Stuart's explicit permission.
- **Issue/PR comments read like a person wrote them** — a colleague's chat reply that acknowledges what they said, explains the substance, and points at the fix. No bullet recaps, no `**What changed**` / `**Verified**` / `**Why**` headings, no checklist blocks.

### Testing

- **Every bug fix and feature ships with tests**, including failure and edge-case paths: `tests/lib/*.test.ts` (pure frontend helpers), `tests/pages/*.test.tsx` (components), inline `#[cfg(test)] mod tests` (Rust).
- **TDD wherever applicable**: failing test (RED) → minimal fix (GREEN) → refactor; RED and GREEN are separate commits. Never neuter a RED test to make it pass. Concurrency/race bugs need a *concurrent* failing test — a sequential one can't reproduce a lost update (e.g. a poll writer racing an API save).
- **Fix flaky tests, never tolerate them.** Make them deterministic (pinned clocks — see `fixed_48h`/`fixed_72h` in `forecast/planner.rs` — injected `now`, seeded inputs, hermetic servers); never re-run, skip, or widen assertions.
- **Tests never touch the live config dir or `~/.givenergy-local`.** Anything that loads/saves settings, builds `AppState`, or starts the backend uses a unique temp dir via `GIVENERGY_LOCAL_CONFIG_DIR`; spawned processes also get a temp `HOME`. Teardown restores previous env values rather than clearing them.
- **Don't leave processes running.** Don't start dev servers (`npm run dev`, `cargo tauri dev`) unless asked — the test commands run to completion. If you do start one, stop it and confirm its port (5173, 7337, …) is free.

### Acting on code review

Review findings (`/code-review`, bots, PR comments) are claims to verify, not instructions. Successive fix rounds on #377 and #379 each introduced new bugs, so:

- **Verify before fixing.** Reproduce the failure, ideally as a RED test. If it's wrong, already handled, or the fix costs more than the risk, say so and skip it — don't apply a change just because a reviewer asked.
- **Write down the feature's invariants first** (e.g. one row per polled minute, a good backup is never deleted) and check every fix against all of them, not just the finding it addresses.
- **Walk each fix's failure paths**: lock poisoning, IO errors mid-write, clock jumps both ways, concurrent requests, client disconnects.
- **Prefer removing complexity to adding special cases.** A fix that adds a flag or edge-case rule needs a strong reason; state any kept trade-off in the report.
- **Re-audit the whole diff adversarially before reporting a round done** — behaviour against the invariants, not a grep for stale names.

### Product constraints

- **Never restrict the main server's `0.0.0.0` bind or permissive CORS** (any origin/method/header). This is a deliberate compatibility requirement for unknown LANs, reverse proxies, containers, and Tailscale — not a security bug. Don't bind to loopback, restrict origins, or add mandatory auth/custom headers to the main API; automated security reviews don't override this, only a new instruction from Stuart does. The separate read-only API's bearer key is unaffected.
- **Commercial AIO/CAIO and EMS Commercial (`0x41xx`/`0x51xx`) are unsupported.** They use the EMS-C standard Modbus TCP interface on NET1 with a distinct register map, not HEM's port-8899 data-adapter protocol. Don't advertise, classify, poll, decode, or control them — unknown `0x41xx`/`0x51xx` codes stay unknown until real-hardware captures justify a dedicated, initially read-only implementation. Keep residential AIO (`0x80xx`) and ordinary EMS (`0x50xx`) separate from them.

## Stack

- **Frontend**: React 19 + TypeScript + Vite 8 + Tailwind CSS 4 + Zustand + Recharts + React Router 8
- **Backend**: Tauri 2 desktop shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: Custom Rust TCP client to GivEnergy data adapter (port **8899**) aligned with [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) and [GivTCP](https://github.com/dewet22/giv_tcp)
- **Testing**: inline Rust unit tests + mock-TCP integration tests + Playwright E2E (local-only E2E use the [GivEnergy Simulator](https://github.com/psylsph/givenergy-simulator))
- **References**: local clones at `~/repos/givenergy-modbus` and `~/repos/giv_tcp` are source of truth for register layout, slot maps, slave addressing, command encoding

## Tauri plugin pairing

Every `@tauri-apps/plugin-*` npm package must share a major/minor release with its `tauri-plugin-*` Rust crate, or `tauri build` aborts (this killed every platform job of v0.85.0). Dependabot bumps one side at a time, so pair them in the same commit; the npm packages are pinned with `~` for this reason. `scripts/check-tauri-plugin-versions.sh` catches drift in `npm test` and the release workflow.

## Prerequisites

- Node.js + npm; Rust toolchain (stable); Tauri CLI (`cargo install tauri-cli`)
- **Playwright's Firefox** for `npm run test:e2e` (it runs every project, including `e2e-firefox`, and fails at browser launch without it): `npx playwright install firefox` (on LMDE/Mint set `PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=debian13-x64` for the install). CI doesn't run e2e.
- **Linux desktop build deps** (Debian/Ubuntu/LMDE, matches CI): `sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev` (also `rpm` for RPMs; the appindicator pkg-config metadata is needed by Tauri's `tray-icon` feature)

## Commands

| Command | Action |
|---|---|
| `npm run dev` | Vite dev server on port 5173 |
| `npm run build` | `tsc -b && vite build` (full typecheck + bundle) |
| `npm run lint` | `eslint .` |
| `npm run lint:md` | Markdown lint; run after significant .md edits |
| `npm run check:versions` | Verify `package.json` / `Cargo.toml` / `tauri.conf.json` agree |
| `npm run test` | Vitest + `tests/scripts/*.test.sh` smoke tests |
| `cargo test` (in `src-tauri/`) | Rust unit tests |
| `cargo clippy` (in `src-tauri/`) | Rust linter |
| `cargo tauri dev` / `build` | Dev mode with hot-reload / production build |
| `docker build .` | Container build |
| `npm run test:e2e` | Playwright E2E (needs `npm run build` + `cargo build --release` first) |
| `npm run test:local[:headed]` | Local-only E2E via simulator at `~/repos/givenergy-simulator/target/release/sim-api` |

Full verification order: `cargo fmt --check` → `cargo clippy` → `npm run lint` → `npm run lint:md` → `npm run build` → `cargo test` → `npm run test` → `npm run test:e2e` → `docker build .`. At minimum, run everything up to `npm run test` before committing.

### Dongle misbehaviour tests

`npm run test:local` also runs `e2e/local-dongle-misbehaviour.spec.ts`; each test starts its own simulator + backend with `--dongle-misbehaviour <mode>`, exercising the backend's per-block retry and sanitization layers: `DropConnection` (→ Reconnecting, then reconnects), `Intermittent` (~50% zeros; stays Connected, valid data eventually), `EmptyData` (all-zero registers; snapshot shows zero power fields), `StaleData` (frozen values across polls), `GarbageData` (random u16s rejected).

## Linting rules

- **Rust**: zero clippy warnings; fmt-clean — run `cargo fmt` and fix pre-existing drift in touched files rather than working around it.
- **TypeScript/ESLint**: `verbatimModuleSyntax` — `import type` for type-only imports; `erasableSyntaxOnly` — no `enum`, `namespace`, or constructor parameter properties; `noUnusedLocals`/`noUnusedParameters`; `react-hooks/set-state-in-effect` — no direct setState in `useEffect`, use key-based remounting or derived values.

## Coverage

From `src-tauri/`: `cargo +nightly llvm-cov --no-default-features` (add `--show-missing-lines` for uncovered lines inline, `--html` for `target/llvm-cov/html/`, `--lcov --output-path lcov.info`, or `--all-targets` to include integration tests). Needs nightly `llvm-tools-preview` + `rustfmt` components and `cargo-llvm-cov`.

Pitfalls: never drive the pipeline by hand with `RUSTFLAGS="-C instrument-coverage"` (the test binary's profraw is never written, so every file reports 0%) — `cargo-llvm-cov` wires `RUSTFLAGS`/`LLVM_PROFILE_FILE` correctly. `--no-default-features` is required (the `tauri` feature pulls webview deps and slows the run).

`tests/e2e_mock.rs` is the in-process HTTP integration harness (`tower::ServiceExt::oneshot` against `server::create_router`, no socket bind) — use it as a template for cheap API coverage. `tests/headless_smoke.rs` spawns the real `--headless` binary on an ephemeral port and skips cleanly (printed reason) when the binary isn't built; it always kills and reaps the subprocess.

## Architecture

### Frontend (`src/`)

Entrypoint `src/main.tsx`. **Pages**: Status, Battery, History, Forecast, Control (model-aware rate scaling, slot-labelling warnings), Inverter, Power, Solar, Meters, Octopus (hidden until configured), Settings, Logs (developer mode only). **Components**: `EnergyOrbitDiagram` (radial SVG power flow), battery panels/gauges, charts, `ColdBatteryWarning`. **Hooks**: `useWebSocket` — connects to `/ws`, reconnects on drop, fetches snapshot via REST. **Lib**: `api.ts`, `format.ts`, `types.ts`, `evcLabel.ts`, `validators.ts`. **State**: Zustand `useInverterStore` — snapshot, connectionState, developerMode (persisted), EV Charger state incl. the `evcEverConnected` latch distinguishing "charger was here, now offline" (`Disconnected`) from "never reached" (`Not Found`); `resetEvc()` clears it when the user saves a new host. The frontend talks exclusively to the local Axum server — never directly to the inverter.

### Backend (`src-tauri/src/`)

- **`lib.rs`** — Tauri setup + headless CLI; spawns Axum server + Modbus poll loop. Two tracing layers: stdout `fmt` (WARN, `RUST_LOG` override) and `LogCaptureLayer` into the in-memory `LogRing` (runtime-adjustable via `PUT /api/log-level`).
- **`history/`** — SQLite (`~/.givenergy-local/history.db`); `HistoryDb`, time-bucketed `query_history()` (AVG, or MAX for cumulative fields), `insert_reading()` (raw row for seeding/imports). The poll loop records via `minute.rs` (issue #379): `queue_poll_reading()` → `flush_poll_rows()` on the blocking pool writes one row per wall-clock minute (power/voltage/temperature averaged; SOC and `today_*` from the latest poll), rewritten with the full aggregate when the next minute starts; `close_open_minutes()` writes the open minute on shutdown (`flush_history_on_shutdown`, bounded to 5 s headless). Invariants: rows land in poll order (queue taken under the connection lock); the writer replaces only rows it inserted itself and never overwrites another row at the same second, so existing history is never deleted (a restart may leave two rows in one minute).
- **`inverter/`** — `model.rs` (`InverterSnapshot`, `DeviceType` Gen1-4 / AC-coupled / three-phase / AIO / HV Gen3-4 / EMS, with model-aware slave addresses, poll blocks, slot counts, battery-protocol selection); `decoder.rs`/`encoder.rs` (register ↔ snapshot / `ControlCommand` → whitelist-validated writes); `sanitizer.rs` (corruption defense, see below); `poll.rs` (drain writes → read → sanitize → broadcast; dongle memory-leak fingerprinting, model-aware slave switching, carry-forward, LV battery protocol at 0x32+ vs HV at 0xA0→0x70+/0x50+); `discovery.rs` (network scan validating the 0x5959 magic header); `state_machines.rs` (connect/reconnect, battery protocol, tariff automation).
- **`modbus/`** — `client.rs` (`ModbusClient`: FC6 writes, stale-frame drain, dongle heartbeat echo, default slave `0x11`; `read_all_with_extras()` picks optional blocks by device type); `framer.rs` (proprietary MBAP + transparent sub-frame + CRC); `registers.rs` (addresses, poll blocks, safe-write whitelist, HHMM codec; per-battery BMS reads and HV stack reads polled separately from `STANDARD_POLL_BLOCKS`).
- **`server/`** — Axum: `api.rs` (REST), `ws.rs` (snapshot stream), `logs.rs` (`GET /api/logs`), `mod.rs` (router + graceful bind). EVC endpoints: `/api/evc/discover`, `/api/evc/status` (reachability + cached snapshot, seeds `evcEverConnected` on page load).
- **`evc/`** — EV Charger client (standard Modbus TCP port 502); broadcasts `PollMessage::Evc` per snapshot, `EvcConnected` on TCP handshake, `EvcDisconnected` on invalid host or connect failure (also clears `latest_evc` so `/api/evc/status` reports `reachable=false`).
- **`settings/`** — persisted JSON (`~/.givenergy-local/settings.json`).
- **`forecast/`** — solar forecast (issue #283): hourly Open-Meteo fetch on a 3h tick riding the weather loop, persisted to `history.db::forecast_values`; self-calibrating site performance ratio (median daily actual-vs-insolation, ≥5 usable days); `consumption.rs` hour-of-day profile (median + p25/p75); `simulate.rs` hourly battery SOC projection; `planner.rs` cheapest-window `PlanRecommendation` with forward wrap; `GET /api/forecast/plan` returns the plan plus an `apply` payload mirroring Control endpoints. `SolarForecastProvider` trait allows alternative sources.
- **`octopus.rs`** — Octopus account integration (issue #212): authenticated half-hourly import/export/gas intervals stored separately from inverter history (supplier data arrives late and may be corrected; gas units preserved as returned — SMETS1 kWh vs SMETS2 m³); powers `/api/octopus/*` (billing costs, HEM-vs-supplier comparison, CSV/PDF export). Cosy/Agile tariff automation is separate, riding the poll loop via `state_machines.rs`.
- **`octopus_dispatches.rs`** — Intelligent Octopus Go dispatch hold (issue #394), opt-in from the Octopus page: polls `flexPlannedDispatches` for the account's live EV devices every 3 min (token reused 50 min, device list 6h) and, while a *smart* dispatch is running by a list ≤ 15 min old, `DispatchHoldState` in `state_machines.rs` pauses discharge as another `Safety` owner of the shared Eco Paused state. Boosts are never held; a failed or stale read means normal behaviour. `evaluate_pause_limiters()` runs temperature → dispatch → load so a dispatch starting as the load limiter releases keeps the saved reserve.
- **`alerts/`** — evaluates each sanitized snapshot against user thresholds (battery temp/SOC, solar clipping, inverter battery-warning flag, grid offline) with per-type cooldown + consecutive-read confirmation; delivers via Telegram Bot API, ntfy (incl. self-hosted), Pushover; daily consumption report; Telegram `/status` `/today` `/report` commands. Covers issue #85. `alerts/battery_health.rs` (issue #393): `run_soc_watch` feeds every sanitized snapshot to `inverter/soc_watch.rs` (`SocWatch`: SOC jumps beyond `jump_allowance` held for 3 polls; SOC held while net battery energy reaches 15% of capacity; resets on gaps longer than 10 min and 3 poll intervals, backwards clocks, the grace period and HR 29 calibration; a poll whose readings exactly repeat the last — stale dongle data, or a module the sanitizer carried forward — is ignored), counts events into `battery_health_daily.soc_jumps`/`soc_stuck` and notifies (jumps ≤ 1 per battery per 6 h; an undelivered message is resent every 5 min for up to an hour via `SocOutbox`, so events are never counted twice); `run_spread_check` runs `history/battery_health_trend.rs::spread_trend` (near-full spread, last 7 days vs 31–60 days ago) once a day after 07:00 (retrying after 10 min when it can't read the DB or deliver), re-alerting only on +15 mV or after 30 days, state in `meta` key `battery_health_alerts`. Read-only; toggles `cell_spread_alert_enabled` / `soc_anomaly_alert_enabled` (default on).
- **`inverter/calibration.rs`** + **`server/calibration.rs`** — guided battery calibration cycle (issue #418), single-phase LV models only (`supports()`; the All-in-Ones are HV and excluded): export at full rate to 4%, charge at full rate to 100%, hold (default 2 h, 30 min–8 h), restore. `build_plan()` fixes each phase's writes and the restore (Force Discharge + Force Charge stop writes from one capture, last write per register, plus reserve and rates) at start, refusing anything it couldn't write back. Each poll `step()` re-applies the phase's writes when `snapshot_matches_writes` says they haven't read back (at most every 5 min), so restarts and limiter pauses heal the same way; a stop near the floor (≤10%, ≤50 W for 15 min) counts as empty; 24 h per phase then restore-and-fail; restore is confirmed only by a read newer than the last restore write (6 attempts). The run persists in `Settings::battery_calibration_run` under the run lock (`advance()`), the outcome in `battery_calibration_last`, with a notification. `DischargeControlOwner::Calibration` sits between Manual Force and Safety. `GET`/`POST /api/battery/calibration`, `POST /api/battery/calibration/cancel`; start refuses while Force, pause, a limiter, the dispatch hold, Cosy/Agile slots or a Timed Export window hold the battery.
- **`update.rs`** — GitHub Releases poll every 6h (gated by `check_for_updates`), cached in `AppState::update`; `GET /api/latest-version` triggers a non-blocking refresh when stale (60s min interval; gated on `loop_registered` so tests stay hermetic).
- **`backup.rs`** + **`history/backup.rs`** — scheduled `history.db` backups (issue #379) via `VACUUM INTO` on a separate read-only connection (never holds the `HistoryDb` lock), integrity-checked as a hidden temp file, then renamed to `history-YYYYMMDD-HHMMSS.db` (UTC) in `backups/` under the config dir or a user-chosen folder. Safety invariants: rotate to `keep` (default 3) only after a success, ignoring files stamped later than the new backup (a wrong clock never destroys a good backup); skip with an error when free space < 1.5× the DB size; the optional `settings.json` copy is off by default (plaintext credentials) and its failure removes the history copy too; a custom folder must already exist and must not be inside `AppState::served_dir`. Status persists in `meta`; the first scheduled failure in a row alerts. `GET`/`POST /api/backup`, `POST /api/backup/run` (202 started, 409 if already running; Settings → History Backup polls `GET` while `running`).

### Shared state (`AppState`)

`Arc<Mutex<…>>` shared between poll loop, API handlers, and WS: `latest_snapshot`, `connection_state`, `pending_writes`, `write_notify` (wakes poll loop), `settings`, `history`, `log_ring` (2000 entries), `update`.

## Data sanitization (register corruption defense)

GivEnergy dongles frequently return corrupted register values; `inverter/sanitizer.rs` defends in layers.

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

Connect → 500ms delay → drain TCP → 1× warmup read (discarded) → clear latest_snapshot → 3 grace readings (absolute checks only; cumulative counters median-of-3 on the final grace reading) → full absolute + delta checks.

History API uses MAX (not AVG) for cumulative counters — AVG understates monotonic values; the frontend `removeSpikes()` in `HistoryPage.tsx` applies a post-query spike filter.

## Modbus write protocol

- **FC6** (Write Single Holding Register) — one register per request; **CRC**: `CrcModbus(function_code + register + value)`
- **Default device address `0x11`** — switches to `0x31` for AC-coupled/Gen1 after detection
- **Slot clearing**: write `0` (`00:00–00:00` = disabled)
- **Retry**: 6 attempts with 2s delay on exception 67 (dongle busy)

### Model-specific write targets

| UI control | DC hybrid (Gen2/Gen3+) | AC-coupled | Three-phase / HV |
|---|---|---|---|
| Charge power limit | HR111 (0-50%) | HR313 (1-100%) | HR1110 (1-100%) |
| Discharge power limit | HR112 (0-50%) | HR314 (1-100%) | HR1108 (1-100%) |
| Battery SOC reserve | HR110 | HR110 | HR1109 |
| Charge target SOC | HR116 | HR116 | HR1111 |

**The DC-hybrid HR111/112 value is a percentage of battery *capacity* (50 = 0.5C), not of the inverter's maximum** (GivTCP `write.py`: `watts / (capacity / 2) * 50`; `read.py`: `min(reg / 100 * capacity_w, inverter_max)`). The Control page, Adaptive Charge, the forecast and the plan-apply path all convert through `src/lib/powerLimit.ts` / `src-tauri/src/inverter/power_limit.rs`; never hard-code a "halve the percentage" mapping (issue #346). 100% writes 50 (no limit). The direct banks (HR 313/314, HR 1110/1108) are plain percentages of the inverter maximum.

API routes inspect `device_type` to choose the command; `ControlPage.tsx` picks the register max (50 vs 100) and display formula. Known limitation: register 32 (charge slot 2 end) returns exception 67 on some inverters, though `enable_charge` still updates correctly.

## Battery power sign convention

HEM convention, uniform across device families and frontend: **`battery_power` positive = discharging, negative = charging.**

| Path | Raw register | Raw wire sign | Decode action |
|---|---|---|---|
| Single-phase | `p_battery` IR(52) | **+ = discharge** (reference) | verbatim |
| Three-phase / HV | `p_discharge - p_charge` (IR 1136-1139) | derived | computed, + = discharge |
| Gateway aggregate | `p_aio_total` IR(1702) | **+ = charging** (opposite!) | **negate** |
| Gateway per-AIO | `p_aioN_inverter` IR(1816-1818) | **+ = charging** | **negate** |

The gateway exception is confirmed by `GivTCP/read.py:1556` (`Battery_Power = -GEInv.p_aio_total`). Forgetting the negate inverts the battery arrow AND derived grid power (`grid = solar + battery − home`), producing impossible readings (issue #78). See the `sign_convention_gateway_*` tests in `decoder.rs`.

## Headless server mode (Linux)

```bash
npm run build && (cd src-tauri && cargo build --release)
./target/release/givenergy-local --headless [--port 8080] [--dist /path/to/dist]
```

`--dist` search order: arg > `./dist/` (cwd) > `<exe_dir>/dist/` > `/usr/share/givenergy-local/dist/`; API-only if none found.

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

Three-phase slot/target registers mirror the single-phase layout at HR 1080-1124; EMS plant-level scheduling uses HR 2040-2071. **The Gateway is single-phase-class for control** (issue #149): Quick Actions / schedules write the standard HR 94/95, 56/57, 96, 116 registers (forwarded to child AIOs), not the three-phase bank nor the EMS schedule — though it *does* poll HR 2040-2075 for plant-level config read-back. GE Cloud UI labels slots in opposite order; the data is identical. `ControlPage.tsx` shows callout banners for the slot-naming mismatch (any 2+ slot hybrid) and legacy Gen3 firmware (ARM FW ≤ 302) where extended HR 240-299 may return stale data.

### Discharge slot handling

Saving an enabled Timed Export slot via `/api/control/discharge-slot` writes the slot + target SOC first, then arms Timed Export (`battery_power_mode=0`, `enable_discharge=1`); the button is locked until a slot exists and direct enables without a slot are rejected. Disabling the last slot while armed returns to Eco (clearing all discharge slot registers). Schedule-backup restores write slots before `enable_discharge=1`; on reconnect the poll loop repairs an externally-created armed-but-slotless state.

### Optional block carry-forward

Optional blocks are conditionally polled by device type: `EXTENDED_SLOTS_BLOCK` HR 240-299 (Gen3, AIO, HV Gen3, AC-three-phase), `AC_CONFIG_BLOCK` HR 300-359 (AC-coupled, AIO, AC-three-phase), `THREE_PHASE_HIGH_CONFIG_BLOCK` HR 1000-1079 + `THREE_PHASE_CONFIG_BLOCK` HR 1080-1124 (three-phase control), `THREE_PHASE_INPUT_BLOCKS` ×7 IR 1000-1413, `GATEWAY_INPUT_BLOCKS` ×5 IR 1600-1859. When an optional block read fails, `carry_forward_optional_block_values()` preserves previous values instead of flashing zeros in the UI.

## Known issues

- **GNOME Wayland toolbar icon**: resolved via app-ID matching — `"enableGTKAppId": true` in `tauri.conf.json`; `npm run dev:desktop` refreshes `~/.local/share/applications/com.givenergy.local.desktop` on every dev run. Packaged .deb/.rpm handle it via their own .desktop file.
- **macOS minimum 10.15**: Vite's modern JS output can't parse on 10.14 WebKit (blank screen), so `bundle.macOS.minimumSystemVersion` is pinned to `10.15`.
- **macOS 26.5 blocks ad-hoc signed binaries**: one-time "Open Anyway" for `/Applications`, `xattr -d com.apple.quarantine` for Gatekeeper, and x86_64 crashes under Rosetta (ship aarch64). `launch.command` in the repo root bypasses `/Applications`.

## Release process

1. Bump version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json` — `npm run check:versions` (gating step in CI and `npm test`) fails if the three drift.
2. Re-pin the Proxmox bootstrap: update `SCRIPT_REF` in `scripts/proxmox/create-lxc.sh` to the new `vX.Y.Z`; bump `INSTALLER_SHA256` too (digest of `git show vX.Y.Z:scripts/proxmox/install.sh`) when `install.sh` changed. `tests/scripts/proxmox-lxc.test.sh` fails CI if the pin trails the version.
3. Update `CHANGELOG.md` with a new heading.
4. Commit, then **immediately tag** (`vX.Y.Z`) matching the changelog heading exactly; push both. The `v*` tag triggers `.github/workflows/build.yml`, which runs `check-versions` as a gating job.
5. GitHub Actions builds macOS (ARM + x64), Linux, Windows; platform jobs upload to a **draft** and a final `publish-release` job verifies every installer is present before publishing (issue #291). If it fails, fix and re-run — don't publish by hand.

### Changelog style

The changelog is for users, not developers: short bullets leading with a bold one-line summary plus one or two sentences of substance — what the user will notice, what they can now do, what stops being broken. No register numbers, no algorithm names, no `**What changed**` / `**Verified**` headings, no "Files touched" lists; reference issue/PR numbers only when closing a user-reported issue. Existing entries are the canonical voice.

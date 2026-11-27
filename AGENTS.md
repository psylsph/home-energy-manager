# Home Energy Manager

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## General rules

**The project owner is Stuart** (`psylsph` on GitHub). Comments and replies posted on Stuart's behalf speak as Stuart — never sign or address him by any other name.

### Git and GitHub

- **Never commit, push, tag, or release without explicit permission.** Make the change, verify, report back, then wait for Stuart to say "commit" or "push". Each approval covers one commit or push only.
- **Commit directly on `master`** when permitted — no feature branches or worktrees. Dependabot and outside contributions arrive as PRs; merge them into `master`.
- **No `Co-Authored-By: Claude` (or similar AI attribution) trailers** on commits or PRs.
- **Never close GitHub issues or PRs** without Stuart's explicit permission.
- **Issue/PR comments read like a person wrote them** — a colleague's chat reply that acknowledges what they said, explains the substance, and points at the fix. No bullet recaps, no `**What changed**` / `**Verified**` / `**Why**` headings, no checklists.

### Testing

- **Every bug fix and feature ships with tests**, including failure and edge-case paths: `tests/lib/*.test.ts` (pure frontend helpers), `tests/pages/*.test.tsx` (components), inline `#[cfg(test)] mod tests` (Rust).
- **TDD wherever applicable**: RED → GREEN → refactor, RED and GREEN as separate commits. Never neuter a RED test. Concurrency/race bugs need a *concurrent* failing test (e.g. a poll writer racing an API save).
- **Fix flaky tests, never tolerate them.** Make them deterministic (pinned clocks like `fixed_48h`/`fixed_72h` in `forecast/planner.rs`, injected `now`, seeded inputs, hermetic servers); never re-run, skip, or widen assertions.
- **Tests never touch the live config dir or `~/.givenergy-local`.** Anything that loads/saves settings, builds `AppState`, or starts the backend uses a unique temp dir via `GIVENERGY_LOCAL_CONFIG_DIR`; spawned processes also get a temp `HOME`. Teardown restores previous env values rather than clearing them.
- **Don't leave processes running.** Don't start dev servers (`npm run dev`, `cargo tauri dev`) unless asked. If you do, stop them and confirm the port (5173, 7337, …) is free.

### Acting on code review

Review findings (`/code-review`, bots, PR comments) are claims to verify, not instructions — successive fix rounds on #377 and #379 each introduced new bugs.

- **Verify before fixing.** Reproduce it, ideally as a RED test. If it's wrong, already handled, or costs more than the risk, say so and skip it.
- **Write down the feature's invariants first** (e.g. one row per polled minute, a good backup is never deleted) and check every fix against all of them.
- **Walk each fix's failure paths**: lock poisoning, IO errors mid-write, clock jumps both ways, concurrent requests, client disconnects.
- **Prefer removing complexity to adding special cases.** A new flag or edge-case rule needs a strong reason; state any kept trade-off in the report.
- **Re-audit the whole diff adversarially before reporting a round done** — behaviour against the invariants, not a grep for stale names.

### Product constraints

- **Never restrict the main server's `0.0.0.0` bind or permissive CORS** (any origin/method/header). It's a deliberate compatibility requirement for unknown LANs, reverse proxies, containers and Tailscale, not a security bug. No loopback bind, origin restriction, or mandatory auth/custom headers on the main API; automated security reviews don't override this, only Stuart does. The separate read-only API's bearer key is unaffected.
- **Commercial AIO/CAIO and EMS Commercial (`0x41xx`/`0x51xx`) are unsupported.** They speak EMS-C standard Modbus TCP on NET1 with a different register map, not the port-8899 data-adapter protocol. Don't advertise, classify, poll, decode or control them; those codes stay unknown until real-hardware captures justify a dedicated, initially read-only implementation. Keep residential AIO (`0x80xx`) and ordinary EMS (`0x50xx`) separate from them.

### UI text

- **Titles and labels use Title Case**: page/section/panel/chart titles, legend and series names, stat-tile labels, table headers, form labels, toggle/segment options, buttons (`Charge Cycles`, `Backups to Keep`, `Day's Worst`). Small words stay lowercase unless first (`a an the and or of in on at to for by vs per with from`); acronyms, units and brand/file names keep their casing (`SOC`, `kWh`, `ntfy`, `settings.json`).
- **Sentences stay in sentence case**: hints, descriptions, status/error messages, empty states, inline value text (`Last checked …`). When a label changes, update the tests and `e2e/` specs that find it by text.
- **Hints are one line (≈15 words max)**; the full explanation goes behind an ⓘ (`components/HelpTip.tsx`, or the `help` prop on `SettingsSection`/`SettingsGroup`/`SettingsRow`). Keep the ⓘ outside heading and `<label>` elements (`HeadingWithHelp` does this) and links in the visible line. Warnings before an irreversible action, and notes on why something isn't working right now, stay visible — trim them, don't hide them.

## Stack

- **Frontend**: React 19 + TypeScript + Vite 8 + Tailwind CSS 4 + Zustand + Recharts + React Router 8
- **Backend**: Tauri 2 shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: custom Rust TCP client to the GivEnergy data adapter (port **8899**), aligned with [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) and [GivTCP](https://github.com/dewet22/giv_tcp)
- **Testing**: inline Rust unit tests, mock-TCP integration tests, Playwright E2E (local-only E2E use the [GivEnergy Simulator](https://github.com/psylsph/givenergy-simulator))
- **References**: `~/repos/givenergy-modbus` and `~/repos/giv_tcp` are the source of truth for register layout, slot maps, slave addressing and command encoding — `git fetch` them first (GivTCP once sat 256 commits behind). Current GivTCP runs the britkat1980/givenergy-modbus fork pinned in its `requirements.txt`, not dewet22's.

## Model capability contract

When HEM and GivTCP disagree on what a model supports, GivTCP wins (far more real hardware). `tests/fixtures/givtcp-model-contract.json` records GivTCP's pause support and three-phase layout per model, plus every known HEM or simulator difference (`hem_deviations` / `sim_deviations`, each with an issue and a why). It's checked offline by `inverter/givtcp_contract.rs`, `tests/lib/givtcpContract.test.ts`, the simulator's `crates/sim-registers/tests/givtcp_contract.rs` (an identical copy, compared in `npm test`) and `e2e/local-model-contract.spec.ts`; an unrecorded difference fails, and so does a recorded one that has gone away.

**Always check for drift and fix it.** Run `npm run check:givtcp` (network) at the start of any work touching model support, pause/charge-cap behaviour or the contract, and before a release. If GivTCP has moved, fix it then: update the fixture and the simulator's copy together, then change HEM or record a deviation with an issue and a why. The "contract is out of date" lines printed by `npm run test` come from `tests/scripts/check-givtcp-contract.test.py` exercising failure paths on purpose; its `OK` is what counts.

## Tauri plugin pairing

Every `@tauri-apps/plugin-*` npm package must share a major/minor with its `tauri-plugin-*` crate, or `tauri build` aborts (killed every platform job of v0.85.0). Dependabot bumps one side at a time, so pair them in one commit; npm packages are pinned with `~`. `scripts/check-tauri-plugin-versions.sh` catches drift in `npm test` and the release workflow.

## Prerequisites

- Node.js + npm; stable Rust; Tauri CLI (`cargo install tauri-cli`)
- **Playwright's Firefox** for `npm run test:e2e` (it runs the `e2e-firefox` project too): `npx playwright install firefox` (on LMDE/Mint set `PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=debian13-x64`). CI doesn't run e2e.
- **Linux build deps** (matches CI): `sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev` (plus `rpm` for RPMs)

## Commands

| Command | Action |
|---|---|
| `npm run dev` | Vite dev server on port 5173 |
| `npm run build` | `tsc -b && vite build` (typecheck + bundle) |
| `npm run lint` / `lint:md` | ESLint / markdown lint (run after significant .md edits) |
| `npm run check:versions` | `package.json` / `Cargo.toml` / `tauri.conf.json` versions agree |
| `npm run test` | Vitest + `tests/scripts/*.test.sh` smoke tests |
| `cargo test` / `cargo clippy` (in `src-tauri/`) | Rust tests / linter |
| `cargo tauri dev` / `build` | Desktop dev mode / production build |
| `npm run test:e2e` | Playwright E2E (needs `npm run build` + `cargo build --release`) |
| `npm run test:local[:headed]` | Local-only E2E against `~/repos/givenergy-simulator/target/release/sim-api`, incl. `e2e/local-dongle-misbehaviour.spec.ts` (`--dongle-misbehaviour` DropConnection / Intermittent / EmptyData / StaleData / GarbageData) |
| `npm run mock:octopus -- --with-hem …` | Mock Octopus dispatch list + simulator + throwaway headless HEM on `:7340` for manual checks of anything reading dispatches (e.g. `--slot 23:30-05:30@12 --running 40`); never touches `~/.givenergy-local`; see `--help` |
| `docker build .` | Container build |

**Full verification order**: `cargo fmt --check` → `cargo clippy` → `npm run lint` → `npm run lint:md` → `npm run build` → `cargo test` → `npm run test` → `npm run test:e2e` → `docker build .`. At minimum run everything up to `npm run test` before committing.

**Headless**: `./target/release/givenergy-local --headless [--port 8080] [--dist DIR]`; `--dist` falls back to `./dist/`, `<exe_dir>/dist/`, `/usr/share/givenergy-local/dist/`, else API-only.

## Linting rules

- **Rust**: zero clippy warnings; fmt-clean — fix pre-existing drift in touched files.
- **TypeScript**: `verbatimModuleSyntax` (`import type`); `erasableSyntaxOnly` (no `enum`, `namespace`, constructor parameter properties); `noUnusedLocals`/`noUnusedParameters`; `react-hooks/set-state-in-effect` (no setState in `useEffect` — use key-based remounting or derived values).

## Coverage

From `src-tauri/`: `cargo +nightly llvm-cov --no-default-features` (`--show-missing-lines`, `--html`, `--lcov --output-path lcov.info`, `--all-targets` for integration tests). Needs nightly `llvm-tools-preview` + `rustfmt` and `cargo-llvm-cov`. `--no-default-features` is required; never hand-roll `RUSTFLAGS="-C instrument-coverage"` (profraw never written, everything reports 0%).

`tests/e2e_mock.rs` is the in-process HTTP harness (`oneshot` against `server::create_router`, no socket) — the template for cheap API coverage. `tests/headless_smoke.rs` spawns the real `--headless` binary, skips with a reason when it isn't built, and always reaps it.

## Architecture

**Per-feature behaviour and invariants live in [`ARCHITECTURE.md`](ARCHITECTURE.md).** Read the relevant section before changing a feature and update it in the same change. This is the map.

### Frontend (`src/`)

- **Pages**: Status, Battery, History, Forecast, Control, Inverter, Power, Solar, Meters, Octopus (hidden until configured), Settings, Logs (developer mode). Nav comes from `lib/navItems.ts::visibleNavItems`.
- **Status page**: orbit diagram (`EnergyOrbitDiagram`) or isometric house (`house/`); both build from `hooks/useEnergyFlowViewModel` over `lib/energyFlow.ts` so they always agree (use the `mode`/`socText` fields, never parse `unit`). Schedule cards use `lib/chargeSchedule.ts::batteryEvent`; the car line and Planner line never feed `batteryEvent`. Battery runtime: `hooks/useBatteryRuntime` → `forecast/runtime.rs`. Today stats: `EnergyBalanceBars` over `lib/energyBalance.ts`. Layout must stay unscrolled at 390×763 with Today stats hidden and fit the `tauri.conf.json` window with them shown (e2e checks it).
- **Settings**: one component per section in `components/settings/` on `SettingsSection`/`SettingsGroup` + `useSettingsSave`/`useSettingToggle`; `SettingsPage` does the single `/api/settings` fetch. Test a section by mounting it alone.
- **State**: Zustand `useInverterStore` (snapshot, connection state, developer mode, EVC state with the `evcEverConnected` latch, `meterLabels`). `useWebSocket` streams `/ws` and fetches the snapshot over REST.
- **Read-only `?RO` links**: `navItems.ts::hiddenInReadOnly`, `components/ReadOnlyHidden`, `hooks/useIsReadOnly` — a convenience, not a security boundary.
- The frontend talks only to the local Axum server, never to the inverter.

### Backend (`src-tauri/src/`)

- **`lib.rs`** — Tauri setup + headless CLI; spawns the server and poll loop; stdout tracing (WARN, `RUST_LOG`) plus `LogRing` capture (`PUT /api/log-level`).
- **`modbus/`** — `client.rs` (FC6 writes, stale-frame drain, heartbeat echo), `framer.rs` (proprietary MBAP + sub-frame + CRC), `registers.rs` (addresses, poll blocks, safe-write whitelist).
- **`inverter/`** — `model.rs` (`InverterSnapshot`, `DeviceType` and its per-model capabilities), `decoder.rs`/`encoder.rs`, `sanitizer.rs`, `poll.rs` (drain writes → read → sanitize → broadcast), `discovery.rs`, `state_machines.rs` (connect, battery protocol, tariff automation, pause owners); feature modules `auto_winter.rs` (#515), `plan_replan.rs` (#404), `dispatch_charge.rs` (#434), `export_once.rs`, `schedule_reset.rs` (#397), `calibration.rs` (#418), `soc_watch.rs`, `power_limit.rs`.
- **`history/`** — SQLite `history.db`; `minute.rs` writes one row per wall-clock minute and never deletes or overwrites another writer's row; `backup.rs` (`VACUUM INTO`; a good backup is never rotated away by a wrong clock).
- **`server/`** — Axum router (`mod.rs`, incl. the Gateway write guard), `api.rs` REST, `ws.rs`, plus per-feature handlers.
- **`forecast/`** — Open-Meteo solar forecast, consumption profile (EV charging excluded), `simulate.rs`, `planner.rs`, `export_floor.rs` (#407), `dispatch_load.rs`, `runtime.rs`, `refresh.rs`.
- **`octopus.rs`** (account data, billing) and **`octopus_dispatches.rs`** (IOG dispatch hold #394, dispatch log #406).
- **`alerts/`** — thresholds, Telegram/ntfy/Pushover delivery, battery health (#393), export summary (#499).
- **`evc/`** (EV charger, Modbus TCP 502), **`settings/`** (`settings.json`), **`health.rs`** (`GET /api/health`, #503), **`update.rs`**, **`backup.rs`**.
- **`AppState`** — `Arc<Mutex<…>>` shared by poll loop, handlers and WS: `latest_snapshot`, `connection_state`, `pending_writes`, `write_notify`, `settings`, `history`, `log_ring`, `update`, …

## Data sanitization (register corruption defense)

Dongles often return corrupt registers; `inverter/sanitizer.rs` defends in layers.

| Field | Absolute range | Notes |
|---|---|---|
| `today_*_kwh` | 0–200 kWh | Catches 245, 275, 879, 1010 spikes |
| Battery power | ±10 kW | |
| Grid power | ±15 kW | |
| Solar power | 0–10 kW | |
| Home power | 0–15 kW | EV charging margin |
| Grid voltage / frequency | 180–280 V / 45–55 Hz | |
| Inverter / battery temp | -20–100 °C / -20–80 °C | |
| Battery module voltage | 0–500 V | LV (~48 V) to HV (~345 V) |
| SOC | 0–100 | Also rejects SOC=0 with live power, SOC=100 while fast-charging |

**Delta checks** (after the 3-reading grace period): `today_*_kwh` never decreases except at midnight (decreases < 0.15 kWh are jitter); max increase `elapsed_hours × 10 kW + 1 kWh`, tighter when the previous value is near zero.

**Connect sequence**: connect → 500 ms → drain TCP → 1 discarded warmup read → clear `latest_snapshot` → 3 grace readings (absolute checks only; counters median-of-3) → full checks.

History queries use MAX (not AVG) for cumulative counters; `HistoryPage.tsx::removeSpikes()` filters afterwards. A failed optional block read carries previous values forward (`carry_forward_optional_block_values()`) rather than flashing zeros.

## Modbus write protocol

- **FC6** (one register per request); CRC `CrcModbus(function_code + register + value)`
- **Device address `0x11`**, switching to `0x31` for AC-coupled/Gen1 after detection
- **Slot clearing**: write `0` (`00:00–00:00` = disabled)
- **Retry**: 6 attempts, 2 s apart, on exception 67 (dongle busy). Register 32 returns 67 on some inverters though `enable_charge` still updates.

| UI control | DC hybrid (Gen2/Gen3+, HV Gen3, AIO Hybrid) | AC-coupled | Three-phase |
|---|---|---|---|
| Charge power limit | HR111 (0-50%) | HR313 (1-100%) | HR1110 (1-100%) |
| Discharge power limit | HR112 (0-50%) | HR314 (1-100%) | HR1108 (1-100%) |
| Battery SOC reserve | HR110 | HR110 | HR1109 |
| Charge target SOC | HR116 | HR116 | HR1111 |

**DC-hybrid HR111/112 is a percentage of battery *capacity* (50 = 0.5C), not of the inverter maximum** (GivTCP: `watts / (capacity / 2) * 50`). Always convert through `src/lib/powerLimit.ts` / `inverter/power_limit.rs`; never hard-code "halve the percentage" (#346). The direct banks (HR 313/314, 1110/1108) are plain percentages of the inverter maximum.

## Battery power sign convention

HEM-wide: **`battery_power` positive = discharging, negative = charging.**

| Path | Raw register | Wire sign | Decode |
|---|---|---|---|
| Single-phase (incl. HV Gen3 / AIO Hybrid) | `p_battery` IR(52) | + = discharge | verbatim |
| Three-phase | `p_discharge - p_charge` (IR 1136-1139) | derived | + = discharge |
| Gateway aggregate | `p_aio_total` IR(1702) | **+ = charging** | **negate** |
| Gateway per-AIO | `p_aioN_inverter` IR(1816-1818) | **+ = charging** | **negate** |

Forgetting the Gateway negate inverts the battery arrow and the derived grid power (`grid = solar + battery − home`) (#78; `GivTCP/read.py:1556`; `sign_convention_gateway_*` tests in `decoder.rs`).

## Schedule slot register layout

| Reference library name | Register | UI label |
|---|---|---|
| `charge_slot_1` / `charge_slot_2` | HR 94-95 / HR 31-32 | Slot 1 / 2 |
| `discharge_slot_1` / `discharge_slot_2` | HR 56-57 / HR 44-45 | Slot 1 / 2 |
| `charge_slot_3..10` | HR 246-268 | Slots 3-10 (Gen3, AIO, HV Gen3) |
| `charge_target_soc_1..10` | HR 242, 245, 248..269 | Per-slot target SOC (Gen3) |
| `discharge_slot_3..10` | HR 276-298 | Slots 3-10 (Gen3, AIO, HV Gen3) |
| `discharge_target_soc_1..10` | HR 272, 275, 278..299 | Per-slot target SOC (Gen3) |
| `charge_slot_2_gen3` | HR 243-244 | Gen3 extended copy of slot 2 |
| `3ph_charge_slot_1..2` / `3ph_discharge_slot_1..2` | HR 1113-1116 / HR 1118-1121 | Three-phase slots 1-2 |
| `gateway_ems_charge_slots` | HR 2053-2071 | Gateway / EMS plant-level charge |
| `gateway_ems_discharge_slots` | HR 2040, 2044-2052 | Gateway / EMS plant-level discharge |

- Three-phase mirrors the single-phase layout at HR 1080-1124; EMS plant scheduling is HR 2040-2071. GE Cloud labels slots in the opposite order (same data); `ControlPage.tsx` warns about that and about legacy Gen3 firmware (ARM FW ≤ 302), where HR 240-299 may be stale.
- **The Gateway keeps its own copy of the schedule and battery registers and doesn't pass writes to the AIO** (#428). HEM refuses battery writes there (`DeviceType::accepts_write`: clock, reboot and HR 2071 only), its battery automations sit out (`battery_controls_reach_battery`) and the UI hides them (`deviceHasBatteryControls`); HR 2040-2075 is still polled for read-back.
- **Timed Export**: saving a slot writes slot + target SOC first, then arms (`battery_power_mode=0`, `enable_discharge=1`); enabling without a slot is rejected, and disabling the last armed slot returns to Eco. Restores write slots before `enable_discharge=1`. Outside-window repairs back to Eco wait for two matching reads from different frames (`RepairReads`, #501), so one bad dongle read never reaches the inverter.

## Known issues

- **GNOME Wayland toolbar icon**: `"enableGTKAppId": true` in `tauri.conf.json`; `npm run dev:desktop` refreshes the dev `.desktop` file.
- **macOS**: minimum 10.15 (Vite output won't parse on 10.14 WebKit). 26.5 blocks ad-hoc signed binaries: "Open Anyway" / `xattr -d com.apple.quarantine`; x86_64 crashes under Rosetta, so ship aarch64; `launch.command` bypasses `/Applications`.

## Release process

1. Bump the version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json` (`npm run check:versions` gates CI and `npm test`).
2. Re-pin the Proxmox bootstrap: `SCRIPT_REF` in `scripts/proxmox/create-lxc.sh` → `vX.Y.Z`, and `INSTALLER_SHA256` (digest of `git show vX.Y.Z:scripts/proxmox/install.sh`) when `install.sh` changed. `tests/scripts/proxmox-lxc.test.sh` fails if the pin trails.
3. Add a `CHANGELOG.md` heading.
4. Commit, then **immediately tag** `vX.Y.Z` matching the changelog heading; push both. The tag triggers `.github/workflows/build.yml`.
5. Platform jobs upload to a **draft**; `publish-release` checks every installer before publishing (#291). If it fails, fix and re-run — don't publish by hand.

### Changelog style

For users, not developers: short bullets leading with a bold one-line summary plus one or two sentences on what the user will notice, can now do, or stops being broken. No register numbers, algorithm names, `**What changed**` / `**Verified**` headings or file lists; cite issue/PR numbers only when closing a user-reported issue. Existing entries are the canonical voice.

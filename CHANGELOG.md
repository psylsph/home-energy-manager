# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-05-29

### Added

- History page with 5 metric tabs (Battery, Solar, Grid, Home, Cost) and time-series charts
- SQLite-backed history storage (`~/.givenergy-local/history.db`) — one row per poll cycle
- Aggregated history API endpoint (`GET /api/history?range=24h&fields=soc,battery_power&offset=0`)
- 7 time range selectors (1h, 6h, 24h, 7d, 30d, 6m, 1y) with Older/Newer navigation
- Cost charts using configurable import/export electricity tariffs (£/kWh)
- Energy tariff settings (import/export rates) on the Settings page
- Headless server mode for Linux (`--headless`, `--port`, `--dist` CLI flags)
- 98 Rust unit tests (4 new history tests)

### Fixed

- Windows production builds now serve the frontend correctly from the Axum server
  using Tauri's resource bundling (`bundle.resources`). Previously the relative
  `../dist` path resolved to a non-existent directory in installed apps, causing
  "127.0.0.1 page can't be found" or "Discovery failed — is the backend running?"
  errors.

## [0.4.0] - 2026-05-29

## [0.3.0] - 2026-05-29

### Added

- Non-technical README with download links and quick start guide
- DESIGN.md with full architecture, protocol, and API reference
- App version shown in Settings → About (injected at build time from package.json)
- GitHub repo description, homepage, and topics (searchable)

### Changed

- README.md rewritten for end users — technical details moved to DESIGN.md
- AGENTS.md updated to reflect current architecture (write protocol, Notify, enable flag gating)
- Energy flow diagram: Home on left, Grid on right

## [0.2.0] - 2026-05-29

### Added

- Correct GivEnergy Modbus write protocol: function code 6 (Write Single Register) with
  device address 0x11, per the givenergy-modbus reference library
- Immediate write execution: control changes are applied as soon as queued, not after
  the next poll cycle (using async notification)
- Write-safe register whitelist aligned with givenergy-modbus reference

### Changed

- Charge/discharge slot clearing now writes 0 (per reference library) instead of
  sentinel value 60
- Slot enabled/disabled state is now gated by the global `enable_charge`/`enable_discharge`
  flags — slots show as disabled when the schedule is turned off, even if individual
  register writes failed
- 00:00–00:00 time slots now treated as disabled (matches reference library convention)
- Energy flow diagram: swapped Home and Grid positions

### Fixed

- Write protocol was using function code 0x10 (Write Multiple) with device address 0x32 —
  the dongle only reliably supports function code 6 with address 0x11 for writes
- Stale frame drain before and after writes to prevent poll read failures
- Fast failure on stubborn registers (6 retries, 2s delay) — previously exponential backoff
  could block the poll loop for minutes
- `apiPost` now checks HTTP response status — control errors surface to the user
  instead of being silently swallowed (code review #4)
- HTTP server no longer panics on port bind failure — logs error and returns
  gracefully (code review #6)
- Response CRC validation is now lenient — logged but not rejected — matching
  the reference library which notes response CRC algorithm is unknown (code review #3)
- Frontend ESLint and TypeScript strict-mode compliance
- All CI checks now pass: lint, typecheck, Rust tests (94 passing)

## [0.1.0] - 2025-05-28

### Added

- Real-time inverter monitoring: solar, battery, grid, home consumption
- Radial energy flow diagram with live power flows
- Battery page with per-module breakdown (cell voltages, temperatures, SOC, cycles)
- Battery mode control: Eco, Timed Demand, Timed Export, Pause
- Charge/discharge schedule management (time slots + SOC targets)
- SOC reserve, charge rate, and discharge rate controls
- Auto-discovery of dongle serial number from response frame header
- Network scanner for discovering inverters on the local LAN
- WebSocket real-time data streaming to connected clients
- Persistent settings (~/.givenergy-local/settings.json)
- 94 Rust unit tests passing

### Fixed

- Modbus polling resilience: inter-request delay (150ms), stale response retry (4 attempts),
  transient error tolerance (3 consecutive failures before reconnect)
- TCP buffer drain after connect to flush stale dongle responses
- 500ms post-connect delay for slow dongle initialisation
- Settings partial update: Connect button no longer clobbers refresh interval
- Settings version tracking: poll loop detects host changes and reconnects immediately

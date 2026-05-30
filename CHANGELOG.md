# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - 2026-05-30

Robust data handling release. The GivEnergy data adapter (dongle) frequently
returns corrupted register values — this release adds multiple defense layers
to ensure clean data reaches the charts and cost calculations.

### Added

- **Data sanitization framework**: Multi-layer defense against corrupted register
  values (see AGENTS.md → Data sanitization):
  - Absolute range checks on every reading (grid voltage 180–280V, frequency
    45–55 Hz, daily energy 0–200 kWh, power ±10 kW, temperature bounds)
  - Delta checks with time-based rate limits after 3-reading grace period
  - Monotonic increase enforcement for cumulative counters
  - Midnight rollover detection
  - Near-zero previous baseline handling
- **Connect sequence**: 3 warmup reads (discarded), snapshot reset, 3-reading
  grace period before delta checks activate
- **Database repair migration**: On startup, scans `today_*_kwh` columns for
  corrupted values (decreases or jumps > 2 kWh) and repairs them
- **MAX aggregation for cumulative counters**: History API uses MAX (not AVG)
  for `today_*_kwh` fields — preserves actual counter values at bucket boundaries
- **CI: Customized macOS DMG**: Removes misleading `/Applications` symlink
  (breaks on macOS 26.5+), adds README.txt with install instructions

### Fixed

- **Cost graphs inflated ~1000×**: AVG aggregation of cumulative counters
  understated values; deltas between averaged buckets amplified corruption
- **Screen flashing on inverter disconnect**: `EnergyFlowDiagram`, `BatteryPanel`,
  `SummaryTiles` wrapped with `React.memo` to prevent SVG animation restarts
  on connection state changes
- **Counters stuck at corrupted values**: Dongle returns garbage on first reads
  after TCP connect (e.g. import=0.6 when real=39.0). Multiple fixes: warmup
  reads, snapshot reset on reconnect, grace period, absolute range check
  always active
- **Missing Disconnected broadcast**: Backend now sends Disconnected state via
  WebSocket when reconnect fails (was set locally but not broadcast)
- **Grid voltage/frequency spikes**: 409V and 664V readings from corrupt
  registers now caught and replaced with previous valid values

### Changed

- **Workflow restructured**: Replaced `tauri-apps/tauri-action` with manual
  `cargo tauri build` + `softprops/action-gh-release` to allow DMG customization
- **Removed cost chart "inaccurate" overlay**: Cost data now accurate with
  MAX aggregation and proper sanitization
- **Test count**: 101 tests (was 98)

## [0.8.9] - 2026-05-30

### Fixed

- **Grace period for delta checks after connect**: Even after warmup reads, the
  dongle returns plausible-but-wrong values (e.g. `today_import_kwh = 0.6` when
  real is 39.0) that pass the absolute range check. These become the "previous"
  baseline and cause all subsequent real readings to be rejected. Now the first
  3 readings after connect only have the absolute range check (0–200 kWh) —
  delta checks are skipped until the baseline stabilizes.
- **3 warmup reads instead of 1**: The dongle can return corrupted data for
  multiple consecutive reads after TCP connect. Three warmup reads with 500ms
  gaps give the dongle more time to stabilize before we start recording data.
- **Skip delta when previous is near-zero**: If `prev < 1.0` the delta increase
  check is skipped — a near-zero previous is unreliable (either clamped from
  corruption or a genuine start-of-day reading).

## [0.8.8] - 2026-05-30

### Fixed

- **Absolute range check now runs on EVERY reading, including the first after
  connect**: Previously ALL sanitization was gated behind `if let Some(p) = prev`,
  meaning the first reading after every restart/reconnect had ZERO validation.
  Corrupted values like 1010 kWh (`today_charge_kwh`), 275 kWh
  (`today_consumption_kwh`), and 245 kWh (`today_export_kwh`) sailed through
  and poisoned the "previous" reference, making the sanitizer reject all
  subsequent legitimate readings. Now the absolute range check (0–200 kWh)
  runs unconditionally, and only the delta/decrease checks require a previous
  reading.

## [0.8.7] - 2026-05-30

### Fixed

- **Cumulative counters stuck at 0 or corrupted values**: The dongle returns
  garbage register values on the first read after TCP connect (e.g.
  `today_import_kwh = 0.6` when the real value is 39.0). The sanitizer
  compared every subsequent reading against this corrupted "previous" value
  and rejected the real ones as "jumped too fast", permanently locking the
  counters at the corrupted value. Fixed with three changes:
  1. **Warmup read**: discard the first register read after connect — the
     dongle needs one full read cycle to return fresh data
  2. **Reset snapshot on disconnect/reconnect**: clear `latest_snapshot`
     so the next connection starts with no stale "previous" reference
  3. **Time-based increase threshold** (from v0.8.6): scales the allowed
     jump with elapsed time since last reading

## [0.8.6] - 2026-05-30

### Fixed

- **Time-based energy counter sanitization**: The fixed 2 kWh/poll increase
  threshold rejected legitimate values after reconnect/restart gaps. The counter
  can legitimately increase by ~10 kWh/hour, so after a 4-hour disconnect the
  threshold needs to be ~41 kWh. Now scales with elapsed time:
  `max_increase = elapsed_hours × 10 kW + 1 kWh margin`.
- **Grid voltage sanitization**: Rejects values outside 180–280V (UK grid is
  nominally 230V ±10%). Catches spikes like 409V and 664V from corrupt
  register reads.
- **Grid frequency sanitization**: Rejects values outside 45–55 Hz (UK nominal
  50 Hz ±1%).

## [0.8.5] - 2026-05-30

### Fixed

- **Cumulative counter sanitization rewritten**: The previous sanitizer used a
  50 kWh jump threshold that missed common register corruption patterns like
  39.0 → 0.6 → 39.0 (only a 38.4 kWh drop). The new sanitizer enforces three
  strict rules: (1) value must be 0–1000 kWh, (2) counter must never decrease
  during the day (register corruption), (3) counter must not increase by more
  than 2 kWh between polls (implausible rate). Midnight rollover is correctly
  detected and allowed.
- **Database repair migration**: On startup, the history database is scanned
  for corrupted `today_*_kwh` values (decreases or jumps > 2 kWh between
  consecutive rows) and repaired using windowed analysis. This cleans any
  corrupted data accumulated before the sanitizer was added.

## [0.8.4] - 2026-05-30

### Fixed

- **Cost graphs now accurate**: Switched history aggregation from `AVG` to `MAX` for
  cumulative daily energy counters (`today_*_kwh`). Averaging monotonically-increasing
  counters understated the true value, causing deltas between buckets to inflate costs
  by ~1000×. `MAX` preserves the actual counter reading at each bucket boundary.
- **Removed inaccurate overlay**: Cost charts no longer show the "data may be inaccurate"
  warning banner.
- **Disconnected state broadcast**: Backend now broadcasts `Disconnected` state via
  WebSocket when a reconnect attempt fails (previously set locally but not sent to
  frontend, leaving it stuck on 'reconnecting').
- **Screen flash on disconnect**: Wrapped `EnergyFlowDiagram`, `BatteryPanel`, and
  `SummaryTiles` with `React.memo` so they don't re-render when only connection state
  changes (previously SVG animations restarted on every connection state update).

## [0.8.3] - 2026-05-30

### Fixed

- **macOS Gatekeeper blocking ad-hoc signed app on 26.5+**: When launched via `open`
  or Finder double-click, Gatekeeper silently blocks the web server from starting
  (network entitlements rejected at the LaunchServices level). The app process stays
  alive but never binds to port 7337.
- **macOS 26.5 blocks ad-hoc signed binaries from /Applications**: Even running
  the binary directly from terminal fails if the .app is in `/Applications`.
  Move to Desktop or home folder instead. Updated FAQ and launch.command to
  prefer Desktop over /Applications.

### Added

- **`launch.command`** — Convenience script that runs the app binary directly,
  bypassing LaunchServices Gatekeeper entirely. Drop-in replacement for `open`:
  `./launch.command` from the project root.
- **FAQ: macOS 26.5+ Gatekeeper workaround** — Documents the `launch.command`
  workaround and notes that `spctl --add` is no longer supported.

## [0.8.2] - 2026-05-30

### Fixed

- **Screen flashing on inverter disconnect**: `StatusPage` re-rendered when the
  connection state changed (Connected → Reconnecting), cascading to children
  `EnergyFlowDiagram`, `BatteryPanel`, and `SummaryTiles`. SVG `<animate>`
  elements in the flow diagram restarted their animation on every unnecessary
  re-render, causing a visible "jump". All three components now wrapped with
  `React.memo` to only re-render when the `snapshot` prop actually changes.
- **Missing Disconnected broadcast**: When a reconnect attempt failed, the
  backend set the connection state to `Disconnected` but never broadcast it
  via WebSocket, leaving the frontend stuck on 'reconnecting'.

## [0.8.1] - 2026-05-30

### Fixed

- **Corrupted daily energy counters sanitized**: The six `today_*_kwh` fields (solar,
  import, export, charge, discharge, consumption) are now sanitized before reaching
  the frontend or history database. Values outside 0–1000 kWh or jumping by >50 kWh
  between consecutive polls are replaced with the previous known-good value. This
  prevents garbage register reads (e.g. IR(35)=32230 → 3223 kWh) from appearing as
  absurd spikes on the Home Energy chart.
- **Frontend spike detection for energy fields**: Added spike thresholds (50 kWh) for
  all six `today_*_kwh` fields in the chart spike-removal logic, so any garbage data
  that bypasses backend sanitization is still caught before rendering.
- **Transparent overlay on cost charts**: Cost charts now show a "data may be
  inaccurate" banner as a known-issue warning until the cost calculation is fixed.

## [0.8.0] - 2026-05-30

### Added

- **Peak/off-peak tariff support**: Cost charts on the History page now support
  separate peak and off-peak electricity rates with configurable time windows.
  Settings page shows peak rate, off-peak rate, off-peak start time, and off-peak
  end time for both import and export tariffs.
- **Auto-winter persistence**: The original `enable_charge_target` and `target_soc`
  register values are now persisted to disk before winter mode activates. If the
  app restarts while winter mode is active, the original values are restored from
  disk so they can be written back when the battery warms up.
- **History time window alignment**: History queries now align to hour boundaries
  (1h/6h ranges) or day boundaries (24h+ ranges) instead of using raw wall-clock
  offsets, ensuring consistent data windows across page navigation.

### Changed

- Window height reduced from 1160 to 1024 for better multi-monitor compatibility.
- Tariff config now stored as structured objects (`TariffConfig` with peak/off-peak
  rates and times) rather than a single flat rate, sent via `import_tariff_config`/
  `export_tariff_config` in the settings API.

## [0.7.0] - 2026-05-30

### Added

- **Connected clients display**: The Network Access section on the Settings page now shows
  all connected WebSocket clients with their IP addresses. Local connections (127.0.0.1 or
  the machine's own LAN IP) are labelled "This device".
- **FAQ.md**: Common problems guide covering firewall settings, LAN access, macOS downloads
  (use x64.dmg even on Apple Silicon), network scanning, and finding your inverter's IP.
- **Firewall/connectivity hint**: The "Waiting for data" screen on the Status and Battery pages
  now shows a secondary message suggesting to restart the app and check firewall settings,
  with a link to the FAQ.

### Fixed

- **LAN access in dev mode**: The Axum dev server now serves the built frontend from `dist/`,
  so LAN devices can access the dashboard at `http://<LAN-IP>:7337` instead of getting a 404.
- **Network Access shows LAN IP**: The Settings page Network Access section now displays the
  machine's actual LAN IP (e.g. `192.168.1.x:7337`) instead of `127.0.0.1:7337`. The LAN IP
  is detected from physical network interfaces (excludes Docker, WSL, and virtual adapters).

## [0.6.0] - 2026-05-29

### Added

- **Developer Mode**: New toggle on the Settings page that reveals a Logs page
  in the navigation bar. Logs show captured stdout/stderr output from the
  backend in a scrollable, filterable terminal-style view.
- **Log capture layer**: A `tracing-subscriber` layer captures formatted log
  events into a 2000-entry ring buffer, exposed via `GET /api/logs`.
- Log viewer supports text filtering, level filtering (ERROR/WARN/INFO/DEBUG),
  auto-scroll with manual scroll-to-bottom button, and periodic polling.

### Fixed

- **Network discovery protocol filtering**: The network scanner now sends a
  minimal GivEnergy Modbus read request to each candidate device and verifies
  the response contains the GivEnergy magic transaction ID (0x5959). Devices
  that have port 8899 open but don't speak the GivEnergy Modbus protocol are
  now filtered out from scan results.

## [0.5.5] - 2026-05-29

### Fixed

- **Live snapshot sanitization**: Garbled register values are now corrected *before*
  reaching the frontend, not just filtered from history. When a reading is physically
  impossible (battery power >10kW, SOC=0 with live power, SOC=100 while charging),
  the previous known-good value is used instead. Warns to the log when corrections happen.
- Covers: battery power, SOC, grid power, solar power, and home power — all clamped
  to residential system limits with fallback to the previous snapshot.

## [0.5.4] - 2026-05-29

### Fixed

- **Copy URL button**: Uses `execCommand('copy')` fallback for non-HTTPS (LAN) contexts
  where the Clipboard API is unavailable. Button now stays within panel bounds with
  `shrink-0` and URL text truncates with ellipsis on narrow screens.
- **Removed "QR code coming soon" placeholder** from Settings → Network Access.
- **History data cleanup**: Purged 8 garbled entries — impossible battery power readings
  (20kW+), SOC=100 spikes, and zero-power readings during active charging.
- **BMS SOC validation tightened**: Values outside 1–99 and >30 points from inverter SOC
  are now rejected before recording to history.

### Changed

- **History guard**: Also rejects entries with `|battery_power| > 10000W` (physically
  impossible for residential systems).

## [0.5.3] - 2026-05-29

### Fixed

- **Battery SOC spike to 100%**: BMS module SOC (IR 100) can return garbage values including
  100%. Now validates BMS SOC against inverter SOC (IR 59) — only overrides when within ±30
  points and the value is 1–99.
- **Energy flow diagram z-order**: Cyan animated flow lines could render behind gray track
  lines. Split into two-pass rendering: all gray tracks first, then all animated lines on top.
- **History guard tightened**: Also rejects SOC=100 readings when battery is actively charging
  at >500W (physically impossible).
- **Purged 2 more garbage SOC=100 entries** from history database.

## [0.5.2] - 2026-05-29

### Fixed

- **Battery SOC graph spikes to zero**: IR(59) intermittently returns 0 from the inverter.
  Now uses the more reliable BMS module SOC (IR 100) when available.
- **History recording of garbage data**: Snapshots with SOC=0 but live power telemetry
  are no longer written to the history database.
- **Chart rendering of missing data**: Frontend now uses `connectNulls` and treats missing
  data points as gaps instead of zero, preventing visual dips.
- **Purged 51 bad zero-SOC readings** from existing history database (52% of all records).

## [0.5.1] - 2026-05-29

### Fixed

- Charge/discharge schedule slot editor: Start and End time pickers no longer overlap
  on narrow screens. Stacked vertically instead of side-by-side grid.

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

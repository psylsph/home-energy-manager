# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.25] - 2026-06-02

### Fixed

- **History graphs aligned to local timezone**: The chart domain was aligned to
  UTC midnight using `setUTCHours`, while axis labels displayed local time via
  `toLocaleTimeString`. In BST (UTC+1) this caused the 24h graph to start at
  1am instead of midnight. Changed `alignDown` to use `setHours`/`setMinutes`
  so domain boundaries match the local timezone.

- **SVG crash on corrupted data (React error #31)**: When the snapshot contains
  non-string/number values due to register corruption, the `EnergyFlowDiagram`
  SVG text elements now coerce props to safe types before rendering. Prevents
  the "not a valid string or number" React error that caused a white/blue screen.

- **ErrorBoundary with auto-retry**: Added `ErrorBoundary` around each page
  route so a component crash on one page doesn't take down the entire app.
  Shows a friendly error message with a 30-second auto-retry countdown and a
  manual "Retry now" button. The nav bar and connection indicator stay
  functional during errors.

## [0.9.24] - 2026-06-02

### Changed

- **Schedule timers support 1-minute resolution**: The minute dropdown on
  charge/discharge schedule slots now shows all 60 values (00–59) instead of
  5-minute increments (00, 05, 10, … 55).

## [0.9.23] - 2026-06-01

### Fixed

- **u8 overflow panic in poll loop**: When the dongle returns corrupted data on
  every poll cycle, the sanitizer re-reads immediately but the
  `readings_since_connect` counter (u8) still incremented. After 256 consecutive
  sanitized cycles it overflowed, causing a panic. Changed to `saturating_add(1)`.

- **Suspect auto-discovered serial rejected**: Some dongle firmware versions
  respond to requests with empty serial (10 spaces) but stop responding entirely
  once the serial is set. Auto-discovery from a truncated frame (e.g. 19 bytes
  when 30+ are expected) now marks the serial as suspect — it stays empty for
  all subsequent requests and is not persisted to settings. A warning is logged
  suggesting manual serial configuration.

## [0.9.22] - 2026-06-01

### Fixed

- **HTTP Port Save button invisible**: Button used `bg-accent` which doesn't
  exist in this project's Tailwind theme — the button had no background colour.
  Changed to `bg-flow-active` to match all other primary action buttons.

- **Log line spacing**: Timestamp and level text had a large gap (`gap-3`)
  making them appear disconnected. Reduced to `gap-1` and widened columns
  for a compact, adjacent layout.

- **HTTP Port input white border**: Input border used `border-border-primary`
  which resolved to white on some themes. Changed to `border-transparent`.

- **TCP timeout increased to 15s**: Diagnostics revealed the dongle
  consistently takes ~10.3s per read. The previous 10s timeout was being
  hit by milliseconds on every request, causing constant disconnect loops.
  Increased to 15s to provide adequate headroom.

## [0.9.21] - 2026-06-01

### Added

- **Dynamic log level control**: Developer console now has a Capture Level
  selector (ERROR/WARN/INFO/DEBUG/TRACE) that controls what the backend
  captures via `PUT /api/log-level`. Defaults to INFO; switch to DEBUG to
  see detailed Modbus frame exchange diagnostics (hex dumps, timing, register
  ranges) when debugging connect issues.

- **Modbus frame hex dump**: Each request sent to the dongle is now logged
  at DEBUG level with a hex preview of the first 30 bytes, showing the serial
  number, slave address, and register range in the request.

- **Per-read diagnostic logging**: Register reads log the slave address,
  register type, range, serial number being used, and response timing at
  DEBUG level. Failed requests show the full error message.

### Changed

- **Capture layer now independent of stdout filter**: The developer console
  log ring buffer uses its own level check (inside `LogCaptureLayer`), no
  longer tied to the terminal's `RUST_LOG` env filter. This means the console
  can show DEBUG events while the terminal stays at INFO.

## [0.9.20] - 2026-06-01

### Fixed

- **Ghost battery modules**: Battery probe loop at addresses 0x33-0x37 now
  validates serial number (printable ASCII, ≥4 chars), module voltage (30-65V),
  and calibrated capacity (>0 Ah) before accepting a module. Previously only
  SOC range was checked, which allowed garbage data from non-existent batteries
  to appear as a third phantom module with rubbish values.

- **Poll error swallowing**: Failed poll reads now log the full error message
  (timeout, frame decode, Modbus exception, etc.) instead of silently
  incrementing a failure counter. This makes the developer console actually
  useful for diagnosing connect-read-disconnect loops.

### Changed

- **TCP timeout increased from 5s to 10s**: The GivEnergy dongle has a slow
  processor and on some networks (WiFi bridges, VPNs) 5 seconds was marginal
  for individual frame reads. Both the TCP connect and per-read timeouts are
  now 10 seconds.

- **TCP keepalive enabled**: Connections now use TCP keepalive (10s idle, 5s
  interval) so dead connections (dongle power-cycled, network change) are
  detected promptly instead of hanging until the timeout expires.

### Added

- **Developer console diagnostics**: Warmup reads now log success/failure per
  read. The first successful poll after connect logs SOC and power values.
  Per-block read timing (request/response sizes, round-trip ms) is logged at
  debug level. MBAP header anomalies (wrong transaction/protocol ID, suspicious
  length) are logged as warnings. TCP read errors include the `io::ErrorKind`.

## [0.9.19] - 2026-06-01

### Fixed

- **CI build failure**: Removed unused `serde_json::Value` import that
  caused test compilation to fail.

## [0.9.18] - 2026-06-01

### Fixed

- **Bold axis labels**: History chart axis ticks now correctly render bold
  (Recharts requires `fontWeight` inside a `style` object, not as a direct prop).

## [0.9.17] - 2026-06-01

### Added

- **Chart legends**: Multi-series history charts (Charge/Discharge Power, Grid
  Power, Energy) now show a colour-coded legend so you can tell which line is
  which.

### Changed

- **Chart titles**: Bold and brighter for better readability.

## [0.9.16] - 2026-06-01

### Added

- **RPM package in CI builds**: The Linux GitHub Actions build now produces
  an `.rpm` package (for RHEL/Fedora/openSUSE) alongside the existing `.deb`.
- **unRAID Docker instructions**: Community-contributed guide for running
  GivEnergy Local as a Docker container on unRAID, with persistent data
  and integration into the unRAID Docker UI.

## [0.9.15] - 2026-05-31

### Fixed

- **Slot timers overflow on mobile**: Charge/discharge and Cosy charging
  time pickers used `flex items-center gap-6` which stays horizontal on
  narrow screens. Changed to `flex flex-col sm:flex-row gap-4 sm:gap-6`
  so Start/End fields stack vertically on phones.
- **Cosy toggle wipes charge slots**: The toggle button sent the cosy
  slots state before it loaded from the server (race condition). Added a
  `loaded` gate so toggling is disabled until the fetch completes and
  slots are populated.

## [0.9.14] - 2026-05-31

### Added

- **Inverter Max Output control**: New slider in Control → Battery Limits for
  register 50 (active power rate, 0-100%). Controls the inverter's maximum
  AC output as a percentage of rated capacity.
- **Charge/discharge rate wattage display**: Shows calculated kW alongside the
  percentage (e.g. "37% (3.0 kW)"). Uses the GivTCP formula: percentage of
  battery nominal capacity in watts, capped by the inverter's max rate.
- **Configurable HTTP port**: New `http_port` setting (default 7337) in
  Settings → HTTP Port. Required for running multiple instances on the same
  machine. Frontend dynamically detects the port from `window.location.port`.
- **Developer Console screenshot** added to README.

### Changed

- **Charge/discharge rate defaults**: No longer show misleading 100% before
  the first real snapshot arrives. Displays "—" until inverter data is received.
- **Max battery power per inverter model**: Uses the exact DTC + ARM firmware
  lookup from givenergy-modbus instead of a coarse per-type mapping. Gen1
  AC-coupled inverters (DTC 3001) now correctly show 3000W instead of 5000W.
- **Multi-instance docs**: README now has clear 2-step instructions (separate
  config dir + separate HTTP port) with examples for desktop, headless, and
  Docker.

### Fixed

- **Battery mode flicker**: A single corrupt register read could flip the
  displayed battery mode for one poll cycle. Now requires 2 consecutive
  identical readings before accepting a mode change.
- **Charge/discharge rate range**: Register 111/112 accept 0-100% (not 0-50%).
  The "50% max" in the reference library is a practical recommendation, not
  a register limit. Slider max reverted to 100%.

## [0.9.13] - 2026-05-31

### Fixed

- **Battery module data disappearing**: When a BMS Modbus read fails or returns
  fewer modules than the previous poll cycle, the missing module data was
  completely lost rather than preserved. Added carry-forward logic so the last
  known-good module data (voltage, SOC, temperature, cell arrays) is kept
  until a fresh successful read replaces it. Fixes intermittent empty or
  partial module panels on the Battery page.

## [0.9.12] - 2026-05-31

### Added

- **Battery calibration control** (developer mode): HR(29) register for
  triggering a BMS calibration cycle (discharge → calibrate → charge →
  balance → set capacity). Accessible via Control page when developer mode
  is enabled. Includes confirmation dialog and warning banner.
- **Inverter reboot** (developer mode): HR(163) register for remotely
  rebooting the inverter. Red danger-styled button with confirmation.

## [0.9.11] - 2026-05-31

### Added

- **Battery mode label**: Shows current mode (Eco, Timed Discharge, Paused, etc.)
  below the battery in the energy flow diagram on the Status page

### Changed

- **SOC calculation**: Now trusts the inverter's IR(59) register by default,
  matching the official GivEnergy app and GivTCP. Only falls back to
  capacity-weighted BMS aggregate when IR(59) returns 0 (corrupted).
- **Reserve SOC slider**: Step reduced from 5 to 1 so 4% can be restored
  after changing (previously stuck at 0, 5, 10...)
- **Charge/discharge rate**: Validation expanded from 0-50 to 0-100.
  Some inverters report 100% = 3000W.

### Fixed

- **Slider flicker on save**: Draft value now persists until the snapshot
  confirms the saved value, preventing a flash of the old reading
- **Charge/discharge rate not working**: Backend was rejecting values > 50
  even though many inverters support 0-100% range
- **Missing @types/node**: Added as dev dependency for tsc builds

## [0.9.10] - 2026-05-31

### Added

- **Multi-instance support**: Set `GIVENERGY_LOCAL_CONFIG_DIR` environment
  variable to run multiple copies with separate settings and history.
  Works on Windows (`USERPROFILE`), Linux/macOS (`HOME`), with documented
  examples in README.

## [0.9.9] - 2026-05-31

### Added

- **Battery voltage sanitization**: Rejects corrupt register readings >60V (LV)
  or >400V (HV), falling back to previous valid value
- **ControlPage slider sync**: Sliders now re-sync from the latest snapshot via
  `useEffect`, fixing stale/junk values after tab switches
- **Data accuracy warning**: Subtle disclaimer below the energy flow diagram
  explaining that brief inaccuracies may appear between poll cycles

### Fixed

- **Cold battery warning on startup**: Was showing with default 0°C temperature
  before real data arrived. Now requires temp > 0.1°C to display
- **Charge/discharge rate clamped to 50%**: Registers can return corrupted
  values up to 255; now clamped to the valid 0-50% range
- **CI: Node.js 20 deprecation**: Opted into Node.js 24 via env variable across
  all workflow files

## [0.9.8] - 2026-05-31

### Added

- **Cosy charging mode** (developer feature): Local software-based charge scheduler.
  Inverter stays in Eco mode while the app manages charge timing via ForceCharge
  commands. Up to 3 charge slots with configurable times and target SOC, stored
  locally in settings.json (not written to inverter registers). Supports
  midnight-crossing slots. Toggle auto-saves. Only shown in developer mode when
  Eco is selected.
- **WiFi-UART Server mode advice**: FAQ entry and StatusPage waiting message
  now mention the dongle's WiFi-UART setting must be "Server" not "Client"
  after a factory reset.

### Changed

- **Schedule slot styling**: Charge and discharge schedule editors now use
  the same compact card design as Cosy slots (bg-bg-surface, p-3, smaller
  toggle switch).
- **Charge Schedule hidden during Cosy**: When Cosy charging is active, the
  standard inverter charge schedule section is hidden.
- **Expanded README Quick Start**: Detailed connection instructions with
  screenshots, network scan tip, and macOS caveats.
- **GivEnergy mode buttons simplified**: Timed Demand / Timed Export / Export
  Paused collapsed to just "Timed Discharge" and "Paused".

### Fixed

- **Stale write responses**: Retry now resends the write request after draining
  each stale frame instead of passively consuming retry attempts.
- **Charge slot 2 disable**: Decoder treats start=00:00 as disabled regardless
  of end value, working around unwritable register 32 on some inverters.
- **Timed mode switching**: Removed decoder override that prevented switching
  to timed mode before configuring discharge slots.
- **Aggregate battery SOC**: Multi-battery systems now compute capacity-weighted
  average across all modules instead of using only the first module.
- **PV2 daily energy**: Only included if PV2 voltage > 0 (prevents garbage data
  from phantom second string).
- **ESLint error**: `set-state-in-effect` — derive effectiveMode instead of
  calling setState in useEffect.
- **10 clippy warnings**: derivable_impls, manual_flatten, match_like_matches_macro,
  field_reassign_with_default, empty_line_after_doc_comments, new_without_default,
  same_item_push, manual_clamp.
- **CI native bindings**: Added all platform variants of @rolldown/binding,
  lightningcss, and @tailwindcss/oxide as optional dependencies so builds work
  on macOS/Linux when lockfile was generated on Windows.

## [0.9.7] - 2026-05-30

### Added

- **Aggregate battery SOC**: For multi-battery systems, SOC is now calculated
  as `sum(remaining_capacity_ah) / sum(capacity_ah) × 100` across all modules
  instead of using only the first module
- **Linting tooling**: Added `markdownlint-cli`, `.markdownlint.json` config,
  and `npm run lint:md` script; updated AGENTS.md with full linting rules for
  Rust (clippy), TypeScript (ESLint), and Markdown
- **README**: Expanded Quick Start with detailed connection instructions and
  prominent Getting Started section

### Fixed

- **Stale write responses**: When stale read responses (function codes 0x03/0x04)
  arrive during a write, the request is now resent after draining each stale frame
  instead of passively consuming retry attempts
- **Charge slot 2 disable**: Register 32 (charge slot 2 end time) is unwritable on
  some inverters. The decoder now treats start=00:00 as disabled regardless of end
  value, so writing just the start register is sufficient
- **10 clippy warnings**: empty_line_after_doc_comments, field_reassign_with_default,
  manual_flatten, match_like_matches_macro, derivable_impls, new_without_default,
  same_item_push, manual_clamp
- **ESLint error**: `set-state-in-effect` — derive `effectiveMode` from
  `requestedMode` and `currentMode` instead of calling `setState` in `useEffect`
- **Markdown formatting**: blanks around lists and fenced code blocks, multiple
  consecutive blank lines across all .md files

## [0.9.6] - 2026-05-30

### Added

- **Battery mode selector**: Top-level Eco/Timed toggle with contextual sub-buttons
- **Mode change feedback**: Shows "Settings are being applied — this may take up to 30 seconds"
  while waiting for inverter confirmation, with optimistic UI updates
- **Tooltips on battery mode buttons** explaining what each mode does

### Changed

- **Simplified timed modes**: Collapsed Timed Demand / Timed Export / Export Paused
  to just **Timed Discharge** and **Paused** — the three-way distinction was confusing
  and the practical difference was minimal
- **Schedule slot time pickers**: Start and End now side-by-side on same row with
  tighter label spacing; minute granularity changed from 15 to 5 minutes
- **Charge Schedule section** now visible in both Eco and Timed modes

### Fixed

- **Charge slot 2 disable**: Register 32 (charge slot 2 end time) is unwritable on some
  inverters. A slot is now disabled when start=00:00 regardless of end value, so writing
  just the start register is sufficient
- **Timed mode switch failing**: Removed decoder override that reverted TimedDemand→Eco
  when no discharge slots were configured, which prevented switching to timed mode
  before setting up a schedule

## [0.9.5] - 2026-05-30

### Changed

- **Schedule slot time pickers**: Start and End times now shown side-by-side
  on the same row with tighter label spacing and clearer visual separation
- **Minute granularity**: Changed from 15-minute to 5-minute increments for
  charge/discharge schedule slots

## [0.9.4] - 2026-05-30

### Added

- **Release download guide**: GitHub release page now shows a table explaining
  which file to download for each platform (macOS Apple Silicon, macOS Intel,
  Linux, Windows) with a note about the `/Applications` issue.

## [0.9.3] - 2026-05-30

### Fixed

- **macOS DMG build: hdiutil auto-appends .dmg extension**: `hdiutil create`
  automatically adds `.dmg` to the output filename, so the `mv` to rename the
  temp file failed because the actual file was `.tmp.dmg` not `.tmp`.

## [0.9.2] - 2026-05-30

### Fixed

- **macOS DMG build: read-only mount**: The DMG customization step tried to
  delete the `/Applications` symlink directly on the mounted DMG, which is
  read-only. Now copies contents to a writable staging directory first, modifies
  there, then rebuilds the DMG from the staging directory.

## [0.9.1] - 2026-05-30

### Fixed

- **macOS DMG build: unmatched double-quote in shell script**: The `Customize macOS DMG`
  workflow step had a trailing `"` on the `DMG_PATH=` line, creating an unmatched
  double-quote that caused bash to scan to EOF looking for its pair. This prevented
  the DMG from being customized (no README.txt, /Applications symlink not removed)
  and caused the macOS release builds to fail.

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

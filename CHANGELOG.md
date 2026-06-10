# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.17.24] - 2026-06-10

### Fixed

- **Load discharge limiter logs now visible at default log level** — The
  reset and progress logs were at `debug!` level, so they were silently
  swallowed unless you bumped to INFO. Now at `info!` so you see when
  the count resets, when it's counting down, and when it re-escalates.
  Also added periodic progress logs every ~20% of the delay so you can
  see the count ticking down without waiting in silence.

## [0.17.23] - 2026-06-10

### Fixed

- **Battery symbol layout on the status screen** — The battery node in the
  energy flow diagram now has evenly-spaced labels (mode, SOC, power, and
  the battery name) so nothing looks squashed or floating. Mobile and
  desktop both render the same way.

## [0.17.22] - 2026-06-10

### Fixed

- **Battery temperature now always comes from the battery itself** — Every
  inverter model now reads temperature from the BMS module average instead of
  relying on the inverter's own (often stale) register. HV stacks were using
  the BCU cluster IR(68) which can sit on garbage for hours on some firmware
  versions. Capacity, voltage, current, and SOC still come from the BCU.

- **Power sanitisation now consistent across all fields** — Battery, grid,
  solar, and home power all use the same logic for detecting and recovering
  from corrupted reads, instead of battery/grid/solar using a simpler
  (no-release) check. Less chance of a glitchy reading freezing your display.

### Added

- **PWA remote access guide** in the README — if you want to check your
  system from your phone while you're out, there's now a step-by-step guide
  covering Tailscale, Funnel, and home-screen install.

## [0.17.21] - 2026-06-10

### Added

- **iOS home screen polish** (suggested by [@jammmyb](https://github.com/jammmyb)):
  - Proper Apple touch icons so the app icon doesn't look like a generic
    screenshot when you save it to your home screen
  - Meta tags for standalone PWA mode — no browser chrome, feels like a real app
  - Safe area insets so the header and nav bar don't hide under the notch or
    rounded corners on modern iPhones
- **Energy flow diagram looks better** — Bigger bubbles, larger text, flow
  lines that get thicker when lots of power is moving, arrows you can actually
  see. The solar bubble no longer clips off the top edge on smaller screens.

## [0.17.20] - 2026-06-10

### Added

- **Clickable chart legends** (contributed by [@mikemonkers](https://github.com/mikemonkers)):
  Click a legend label to mute that series — handy when you want to focus on
  just grid or just solar without changing the chart config.
- **Shared time range** between Power and History pages — switch to 6h on
  one page and it follows you to the other. Persists across reloads too.
- **5 new end-to-end tests** covering legend behaviour, range sharing, and
  reload persistence — all running against the real GivEnergy simulator.

### Accessibility

- **Time range buttons now announce their state** — screen readers will tell
  you which time range is currently selected.

## [0.17.19] - 2026-06-09

### Added

- **CT clamp configuration card on the Meters page** — Shows whether your
  external CT ammeter is enabled, whether it's installed reversed, and
  what type of meter is connected. If the ammeter is off, a hint tells
  you how to turn it on. Should help a lot with "why are my meters not
  showing anything" questions.

### Changed

- **Windows builds now ship as MSI only** — the `.exe` installer is gone.
  MSI gives you a proper install/uninstall experience.

## [0.17.18] - 2026-06-09

### Changed

- **Battery temperature now reads from the BMS, not the inverter** — The
  inverter's temperature register (IR 56) is notoriously unreliable across
  all models, not just three-phase. The app now takes the average module
  temperature from the BMS instead, which is what your battery actually
  feels. Falls back to IR 56 only if the BMS isn't responding.

## [0.17.17] - 2026-06-09

### Changed

- **Cosy charging now writes into the inverter's own schedule** — Instead of
  keeping the charge state purely on the app side (which meant a crash would
  leave the battery stuck force-charging), Cosy now loads the current slot
  times directly into the inverter's schedule registers. If the app falls
  over mid-slot, the inverter just keeps following the schedule.

- **Next Cosy slot preloaded before it starts** — The poll loop writes the
  upcoming slot's times into the inverter registers ahead of time (with the
  enable flag off). If the app crashes between slots, the inverter picks up
  the next charge window on its own. Only re-writes when the slot index
  actually changes, so no pointless Modbus chatter.

### Fixed

- **Gen2 Hybrid now correctly limited to 1 charge slot** — Gen2 inverters
  have the register for a second slot but the firmware ignores it. Both
  GivEnergy's own app and GivTCP only show one slot for Gen2, so the app
  now matches.

## [0.17.16] - 2026-06-09

### Changed

- **SOC values below 4% are now caught and corrected on read** — The Control
  page sliders already stopped you writing unsafe values, but the decoder
  now also clamps any reading below 4% to the safe floor. This keeps
  garbage register reads from showing up as 0% or negative in the UI.

- **Discharge schedule always visible, even in Eco mode** — You can now
  configure your discharge slots ahead of time without switching modes.
  Edits stay local until you switch to Timed mode, which prevents the
  Gen3 firmware quirk where setting non-zero slot registers auto-enables
  unrestricted export. The Timed button stays locked until you've set at
  least one slot, and when you do switch, slots + mode flag are sent to
  the inverter in one shot so it never sees enable_discharge=1 without
  slot constraints.

- **Timed Discharge tooltip clarified** — Now reads "Battery covers home
  demand automatically, plus follows your export schedule during slot
  times" to make clear you don't lose automatic home coverage when using
  timed export.

## [0.17.15] - 2026-06-09

### Fixed

- **Three-phase grid voltage no longer rejected as out of range** — The
  voltage check was hardcoded for single-phase (180–280V), but three-phase
  models report ~415V line-to-line. Every poll cycle was flagging it as
  corrupted and falling back to 230V. Now uses 0–500V for three-phase.

## [0.17.14] - 2026-06-08

### Fixed

- **24h history chart now starts at local midnight** — The x-axis was
  sometimes starting at 01:00 for users in BST because the backend was
  aligning to UTC midnight instead of local time. Your daily chart now
  actually spans your full day.
- **Shorter ranges still trim to first data point** — 6h and 1h views
  start at the earliest available reading so the line reaches the y-axis.

## [0.17.13] - 2026-06-08

### Fixed

- **Gen3 stuck in Timed mode**: Switching to Eco mode now also clears all
  discharge slot registers (HR44-45, HR56-57) to prevent the inverter
  firmware from auto-re-enabling `enable_discharge`. Gen3 retains HR59=1
  when discharge slot registers are non-zero, making Eco mode impossible
  through the standard SetEcoMode command alone.

### Changed

- **Discharge Schedule hidden in Eco mode**: The Discharge Schedule section
  is no longer visible when in Eco mode — only charge slots can be
  configured. Switching to Timed mode shows both charge and discharge
  schedules, reading existing configurations from the inverter registers.
  This avoids the Gen3 issue where configuring discharge slots from Eco
  mode causes the inverter to auto-enable timed discharge.

## [0.17.12] - 2026-06-08

### Fixed

- **Charge slots no longer go blank after mode switches (ECO ←→ Timed)**
  Removed the `enable_charge` gating on charge slot `enabled` state.
  Some inverter firmware clears the master `enable_charge` flag (HR 96)
  during mode transitions. Previously this caused ALL charge slots to
  appear disabled in the UI even though their schedule times were still
  correctly stored in the registers. Charge slots now behave like
  discharge slots — always visible, with `enabled` reflecting whether
  the slot has configured times. The frontend continues to use
  `enable_charge && in_charge_window` to show whether the schedule is
  actively charging, without hiding the configuration.

## [0.17.11] - 2026-06-08

### Fixed

- **Delta baseline self-correction for cumulative counters**: If a daily
  or lifetime energy counter is consistently reported lower by the
  inverter for 10+ consecutive poll cycles, the system now accepts the
  raw value instead of locking onto a corrupted baseline forever.
  Previously, a single grace-period spike (e.g. reading 7.7 kWh when
  the actual value was 7.1) would cause the sanitizer to carry forward
  the wrong value indefinitely, logging "register corruption" warnings
  every 3 seconds. After 10 consecutive corrections the baseline is
  released at `INFO` level and normal operation resumes.

### Improved

- **Three-phase meter decoder**: Per-phase active power (`p_active_phase_1/2/3`)
  now comes from the dedicated load power registers IR(1083-1085) instead of
  dividing total grid power by 3. Power factor is read from IR(1068) and
  apparent power from IR(1073-1074), replacing synthesised estimates with
  actual register values.

## [0.17.10] - 2026-06-08

### Fixed

- **F5 refresh no longer loses the page (#59)**
  Switched the frontend router from `BrowserRouter` to `HashRouter`. This
  means all page navigation happens after the `#` in the URL (e.g.
  `http://host:7337/#/battery` instead of `http://host:7337/battery`).
  The server only ever sees requests for `/`, so pressing F5 on any page
  now correctly reloads the app instead of hitting a 404 or showing a
  blank screen. This should also fix the issue where updates took up to
  10 minutes to appear in non-incognito browsers — the browser can no
  longer cache stale route responses.

### Roadmap

- Added "Static asset caching headers" as a near-term candidate for
  further improving post-update load times (`Cache-Control` headers on
  static assets).

## [0.17.9] - 2026-06-08

### Fixed

- **Gen3 charge slot 2 register mismatch (#51)**
  On Gen3/AIO/HV-Gen3 inverters, charge slot 2 schedule times were read
  from the classic HR 31-32 registers instead of the authoritative HR 243-244
  extended-block copy. The Gen3 firmware stores the active schedule at
  HR 243-244 (named `charge_slot_2_x` in givenergy-modbus); HR 31-32 may
  contain stale or zeroed data. This caused the slot to display wrong times
  (e.g. `00:01-00:04` instead of the configured `03:15-04:15`).

  **Decoder** (`decode_holding_240_299`): now reads charge slot 2 times from
  HR 243-244 when the device supports Gen3 extended slots and does NOT use
  the three-phase schedule map. The extended-block values override the
  classic HR 31-32 decode.

  **Encoder** (`SetGen3ChargeSlot2`): new command variant that writes to
  HR 243-244 instead of HR 31-32 for affected models.

  **API** (`charge_slot_command_for_device`): routes charge slot 2 writes
  to the Gen3 variant when `device_type.supports_gen3_extended()` and the
  model does not use three-phase schedule slots.

  Cross-referenced against both givenergy-modbus (`charge_slot_2_x` at
  HR 243-244) and GivTCP (RegisterMap.CHARGE_SLOT_2_START resolves to 243
  due to Python class attribute shadowing).

## [0.17.8] - 2026-06-07

### Fixed

- **Read-path now fails fast on NotConnected/SendFailed**
  The catch-all retry arm retried ALL errors including dead connections,
  wasting ~2s (4×500ms) before surfacing the failure. NotConnected and
  SendFailed now return immediately.

- **Three-phase config block extended to cover HR 1000-1079**
  Previously only read HR 1080-1124, missing the high config range
  (HR 1005 REAL_TIME_CONTROL, HR 1078 BATTERY_RESERVE_PERCENT).
  Added THREE_PHASE_HIGH_CONFIG_BLOCK to the poll cycle.

## [0.17.7] - 2026-06-07

### Fixed

- **Exception retry logic no longer retries permanent errors**
  The read-path retry guard checked both `msg.contains("code 67")` and
  `msg.contains("Modbus exception")`. Since every exception message
  contains "Modbus exception", ALL exceptions triggered retry (4×500ms
  = 2s wasted). Now only code 67 (busy/dongle-busy) is retried.

- **Safe-write whitelist expanded to match reference**
  Added ~25 missing registers: HR 1005 (three-phase real-time control),
  1078 (battery reserve), 199 (parallel mode), 331 (force off-grid),
  5010/5014 (restart/calculated load), 554-573 (Smart Load slots),
  and 2053-2071 (EMS charge/export slots).

- **Integer truncation guarded in cosy slot parsing**
  Hours clamped to 0-23, minutes to 0-59 before u8 cast.

## [0.17.6] - 2026-06-07

### Fixed

- **Crash in `/api/logs?after=` handler fixed**
  `LogRing::read_from` had an arithmetic underflow when the ring buffer
  was partially filled (len < capacity). `oldest_idx` computed as 2000
  for a partially-filled buffer, causing `newest_idx - read_from` to
  underflow to ~2^64 and `Vec::with_capacity` to panic. Fixed the
  virtual-index formula to handle the partially-filled case separately
  (entries at 0..len, not at cursor+capacity-len). Added 3 regression
  tests.

## [0.17.5] - 2026-06-07

### Fixed

- **Frame decoder now accepts unit ID 0x00** (reference accepts both 0x00 and 0x01).
- **ForceDischarge clears discharge slot 2** in addition to slot 1, matching GivTCP.
- **Hour/minute validation added to slot API handlers** — hours 0-23, minutes 0-59
  are now enforced before encoding to HHMM.
- **Unknown /api/* paths return 404** instead of serving the frontend index.html.
- **detect_lan_ip() moved to spawn_blocking** — no longer blocks the Tokio worker.
- **Settings save uses fixed temp file name** with orphan cleanup on each write.
- **All `log::` calls replaced with `tracing::`** in discovery.rs and settings/mod.rs
  so debug messages appear in the developer console.
- **Added doc comments** for unverified IR(180-239) range, mixed-lock connected_clients,
  discovery false-positive risk, and AIO voltage discrepancy (307V vs GivTCP's 317V).

## [0.17.4] - 2026-06-07

### Added

- **GET /api/logs now supports `?after=` parameter**
  The documented `?after=<n>` query parameter was ignored — the handler
  always returned `read_all()`. Added `read_from()` method on `LogRing`
  and wired up the `Query` extractor. Returns a `next` field for
  incremental polling.

- **Server errors now return HTTP 500 instead of 400**
  `error_response` always returned `BAD_REQUEST` for all errors including
  backend failures (database errors, save failures). Added `server_error()`
  helper returning `INTERNAL_SERVER_ERROR` so clients can distinguish
  bad input (400) from backend outages (500).

### Changed

- **History repair migration documented as deliberately idempotent**
  The cumulative-counter repair runs on every launch but checks for the
  column first and exits immediately if present — not a performance concern.
  Added doc comment explaining this.

## [0.17.3] - 2026-06-07

### Fixed

- **Frontend now shows detailed API error messages**
  `parseApiResponse` was checking `res.ok` before parsing the body,
  so helpful 400-level error messages like "Charge slot 5 not supported
  on this inverter model" were reduced to "API error: 400". Now extracts
  the `{error}` field from the body before falling back to the status.

- **History SQLite insert moved off the Tokio worker**
  `insert_reading` was called while holding the async history mutex,
  blocking the worker with synchronous I/O every poll. Now clones the
  `Arc<HistoryDb>`, drops the lock, and uses `spawn_blocking`.

- **Integer truncation prevented in slot validation**
  A `u64` from the request body was cast directly to `u8` before
  validation (e.g. `{slot: 258}` → `258u64 as u8 = 2`, writes to slot 2).
  Now validates the original `u64` against `u8::MAX` before truncating.

- **History loading spinner no longer gets permanently stuck**
  If deps changed mid-fetch, the `cancelled` flag skipped the decrement,
  so `loadingKey` grew unboundedly. Moved the decrement into `.finally()`
  so it always runs regardless of cancellation.

## [0.17.2] - 2026-06-07

### Fixed

- **Three-phase battery voltage no longer stuck at 0°C with false winter mode**
  When the HV BCU/BMS read failed, `battery_temperature` was set to 0.0,
  which passed the sanitizer and triggered `check_auto_winter` (0°C < 8°C
  threshold), force-charging the battery purely because temperature data
  was missing. Now uses `NaN` as the sentinel, which comparisons always
  return false for — winter mode never activates on missing data.

- **Single-phase consumption no longer understated by double-subtracting AC charge**
  IR(35) is `e_ac_charge_today` (grid→battery energy), not house consumption.
  Consumption is now computed from the energy balance formula
  (`solar + import - export - ac_charge`), matching the reference library.

- **Lifetime battery charge/discharge energy no longer zero**
  The `IR(180,60)` poll block was never read — it carries alternative total
  battery energy counters. Added to the standard poll blocks and decoded
  into new `total_charge_kwh` / `total_discharge_kwh` snapshot fields.

- **Modbus exception code reading fixed for real dongle frames**
  Exception responses embed the 10-byte serial prefix before the exception
  code; the code was reading `payload[0]` (a serial byte like `'S'=0x53`)
  instead of `payload[10]`. The busy-retry path for code 67 was dead against
  real hardware.

- **TCP stream resynchronisation after stray bytes**
  A single stray byte permanently desynchronised the stream. Now scans for
  the `0x59590001` start marker, discards garbage before it, and recovers
  from split markers across reads — matching the reference framer.

- **HHMM time values now validated before sending to inverter**
  Slot-write commands had no validation on packed HHMM values. A malformed
  value like 1690 (16:90) could reach the inverter. Added `validate_hhmm()`
  to all ten slot-encoding arms.

- **Charge target flag cleared when target SOC is 100%**
  100% means "no limit" — the enable flag should be 0, matching GivTCP's
  reference write pattern.

- **Consective-failure break no longer tears down TCP**
  The `break` on 3 consecutive failures exited the inner loop into the
  disconnect path, forcing a full reconnect (warmup + grace period).
  Now falls through to the sleep-wait section, staying connected.

- **Dongle memory-leak corruption no longer broadcast before re-poll**
  When the corruption fingerprint matched, the snapshot was sanitised,
  stored, broadcast via WebSocket and written to history before the re-poll
  replaced it. Now returns early without broadcasting.

- **Nominal battery voltage corrected for HV device types**
  `HybridHvGen3`, `AllInOneHybrid` and `Gen4Hybrid` fell through to the
  51.2V LV default, producing capacity ~7× too low. Now return the correct
  per-module (76.8V) or stack (307.0V) voltage.

### Performance

- **MAX_REGISTERS_PER_READ increased from 20 to 60**
  Every 60-register block was split into 3 sub-requests, tripling Modbus
  traffic and latency. Now reads 60 per request, matching the reference.
  Saves ~450ms of inter-request delay per poll cycle.

- **Settings loaded once per cycle instead of 5× synchronously**
  `Settings::load()` reads `settings.json` from disk synchronously on the
  Tokio worker thread, and was called 5-6 times per poll cycle across
  auto-winter, cosy and agile sections. Consolidated to a single load.

- **History API query moved to blocking thread**
  The history endpoint held the async lock across synchronous SQLite I/O.
  For 30d/1y ranges this blocked the Tokio worker for hundreds of
  milliseconds. Now runs on `spawn_blocking`.

- **Settings API no longer holds lock across disk I/O**
  Tariff defaults are pre-loaded before the async lock, and the lock is
  dropped before `persist.save()`. The poll loop is no longer starved
  while settings are written to disk.

### Changed

- **API now returns proper HTTP status codes**
  All handlers returned 200 with `{ok:true/false}` in the body — errors
  were indistinguishable by status code. Now returns 400 for application
  errors and 200 for success, so the frontend can branch on the status.

- **In-memory settings updated after disk save succeeds**
  Previously the in-memory copy was mutated first, then `persist.save()`
  ran. If the disk write failed, the poll loop reconnected to the new
  host while `settings.json` held the old one. Now saves to disk first.

- **Auto-winter save failure now returns error**
  `set_auto_winter` logged a warning on save failure but returned
  success, matching `set_cosy` and `set_agile` which already returned
  the error correctly.

- **Port parsing distinguishes "not provided" from "explicitly 0"**
  The port default was 8899, making the `if port == 0` validation dead
  code. Now uses `body.get("port")` to detect explicit omission.

### Fixed (Frontend)

- **WebSocket no longer reconnects after intentional close**
  Cleanup called `ws.close()` which fired `onclose` asynchronously,
  scheduling an orphan reconnect. Now closes with code 1000 (Normal
  Closure) and `onclose` only reconnects for non-1000 codes.

- **Removed dead Desktop Settings toggles**
  "Auto-start on login" and "Minimise to system tray" were bound to
  `checked={false} onChange={() => {}}` — completely non-functional.
  Removed the section.

- **History loading spinner now actually shows**
  `loadingKey` was decremented in the fetch callbacks but never
  incremented before the fetch. Added the increment so the spinner
  displays while data is loading.

## [0.17.1] - 2026-06-07

### Fixed

- **Three-phase battery voltage no longer stuck at 0V**
  The sanitizer capped three-phase battery voltage at 100V, but HV
  stackable batteries operate at 200–600V. A valid reading like 241.9V
  was rejected as out of range and fell back to the previous value (0.0
  on first read), keeping the voltage permanently stuck at zero. Raised
  the three-phase cap to 600V, matching the full HV operating envelope.

## [0.17.0] - 2026-06-07

### Added

- **HV stackable battery support (GIV-BAT-3.4-HV / GIV-BAT-*-HV)**
  Three-phase and HV inverters like the GIV-3HY-11 now show real battery
  readings. These inverters use a completely different battery protocol
  (BCU/BMU cluster at addresses 0x70/0x50) instead of the LV protocol at
  0x32, so the app was simply looking in the wrong place — no data ever
  arrived. The work in this release:
  - 🔍 **Discovery**: the BMS aggregator at 0xA0 is probed once to find
    how many battery stacks are present, then each stack's BCU is read for
    pack-level voltage, current, temperature and capacity.
  - 🔋 **Per-cell detail**: each module in the stack exposes 24 cell
    voltages, 24 cell temperatures and its serial number via the BMU at
    0x50+. The Battery tab now shows this alongside the familiar bar
    chart with a proper Y-axis scaled to the pack's voltage range.
  - 💬 **Heartbeat fix**: the dongle pings the app every ~3 minutes and
    closes the socket after 3 unanswered pings (~9 min). The consumer
    task now echoes those heartbeat frames back — no more mysterious
    reconnects every few minutes.

### Fixed

- **Three-phase batteries now show temperature, capacity and max power**
  On three-phase inverters (like the GIV-3HY-11) the Battery and Inverter
  tabs were showing zeros for battery temperature, stored/available
  capacity and max charge/discharge power. The cause was that those
  values don't exist anywhere in the three-phase inverter's own registers
  — they only live in the battery pack's BMS. The app was reading the
  single-phase registers instead, which are simply not populated on
  three-phase hardware. It now derives temperature, capacity and max
  power from the BMS module data (the same place single-phase gets them
  indirectly). If the BMS read fails or the pack isn't responding, the
  fields now show a clean zero with the inverter's hardware power limit,
  rather than a stale garbage value.

### Changed

- **Far fewer log messages by default**
  Both the terminal/journal output (when running headless) and the
  developer console used to default to the INFO level, which floods
  the logs with routine per-poll lines — useful when debugging, but
  noisy day-to-day and liable to push genuine warnings out of the
  2000-entry developer console ring. Both now default to WARN. You
  still see everything that matters, and can bump either one back up
  for a session (the developer console has its level buttons; the
  terminal takes `RUST_LOG=info` or `=debug`).

## [0.16.0] - 2026-06-07

### Fixed

- **Dongle comms completely rewritten** — The old approach was simple but
  fragile: fire off a request, wait for the reply, and if something went
  wrong (wrong device answered, or a delayed frame from a previous request
  turned up) you'd have to notice, flush the junk, and retry. Miss one and
  everything cascaded into mismatched responses and timeouts — especially
  on AC-coupled inverters where battery modules share the bus. Now there's
  a background listener that reads everything the dongle sends and routes
  each response by what's actually *in* the response (which slave, which
  register range). If a battery module answers when you asked the inverter?
  Nobody asked for that, so it's quietly ignored. This is how the reference
  library always worked, and it's far more solid in practice.

- **Force-charge re-sent on restart when in a Cosy slot** — If the app
  crashed or was restarted during a Cosy charging window, it logged "will
  re-send" but never actually sent the writes. Fixed.

- **Mode switching cleanup** — Switching from Agile to Cosy (or away from
  either) could leave the battery force-charging indefinitely. The exit
  cleanup now runs whenever a mode is disabled, not just when its time
  window ends.

- **Cosy writes were silently failing** — A bug matching write responses
  caused every write to time out even though the dongle acknowledged it.
  This broke Cosy charging, mode switching, and everything else that
  needed a write. Fixed.

- **Settings file no longer gets corrupted by concurrent writes** — If
  two parts of the app wrote to the settings file at the same time, the
  JSON could get mangled. Now saves are serialised and use atomic writes
  (temp file + rename) so readers always see a complete file.

- **"Stop Charge" button now listens to your schedule** — It was lighting
  up even when your battery was just sitting in Eco mode. Now it only
  shows when you're actually inside a charge window.

- **Dongle busy errors won't snowball anymore** — Three busy errors used
  to kill the connection and start a slow reconnect spiral (5s, then 10s,
  then 20s…). Now it shrugs, skips that poll, and tries again on the next
  normal refresh.

- **Charge slots clean up after winter mode** — A stale flag could linger
  and confuse the app. Saving a charge slot now sweeps it away.

- **History chart lines now reach the axis** — The x-axis was starting at
  the top of the hour but data is recorded at 30-minute intervals, leaving
  a gap before the first data point. The axis now starts at the earliest
  reading.

### Added

- **Status page shows which Agile slot is active** — The energy flow
  diagram now shows "Agile: charging" or "Agile: discharging" so you can
  see what the state machine is doing at a glance.

- **Refresh interval now a button group** — Instead of a slider, you pick
  from 5, 10, 15 or 20 seconds. Old saved values get snapped to the
  nearest option.

## [0.15.0] - 2026-06-06

### Added

- **Lifetime total import/export tracking**: New `total_import_kwh` and
  `total_export_kwh` fields decoded from inverter registers (IR 32-33/21-22
  for single-phase, IR 1382-1383/1386-1387 for three-phase). Displayed as
  "Import Total" / "Export Total" on the Inverter page.
- **Lifetime energy sanitisation**: Total import/export values are guarded
  by absolute range checks (capped at 100,000 kWh) and delta checks
  (monotonic increase, time-based rate limits) to reject corrupted register
  reads.
- **Three-phase synthetic meter populated with lifetime totals**: The
  built-in grid CT meter (address 0x00) on three-phase models now carries
  the actual lifetime import/export kWh values from the inverter registers.
- **Per-slot discharge target SOC**: When editing discharge slots on
  inverters that support the extended schedule block (HR 240-299), the
  per-slot target SOC is now written to the inverter.
- **Agile price forecast caching**: Price data is now cached by date and
  fetched with `period_from` anchored to the start of today, so the display
  doesn't get wiped out when the Octopus API switches to publishing
  tomorrow's data (~1-2pm each day).
- **Rolling 24-hour price window**: The Price Forecast grid now shows a
  rolling 24-hour window from the current time, smoothly transitioning
  across the day boundary as slots shift into the past.
- **Agile auto-refresh**: Prices are re-fetched every 5 minutes, and the
  rolling window recomputes every 30 seconds to keep the "now" indicator
  accurate.

### Changed

- **Meters page labels**: "Import Today" / "Export Today" renamed to
  "Import Total" / "Export Total" to reflect that meter data shows
  lifetime totals.

### Fixed

- **Note/warning box text readability in light mode**: All note boxes,
  Beta badges, DEV badges, WARNING, and DANGER callouts on the Control
  page and Cold Battery Warning component now use `text-text-primary`
  (resolves to near-black in light mode) instead of light-tinted colours
  that were invisible on pale backgrounds. The `dark:` media-query variants
  were removed to prevent OS dark mode from overriding the app's own theme
  selection.
- **Active Agile price slot now uses bold red border**: The current
  half-hour slot in the Price Forecast grid now shows a pulsing red border
  (`border-2 border-red-500 animate-pulse`) for clear visibility.

## [0.14.0] - 2026-06-06

### Added

- **Charging Mode selector**: Replaced the Cosy on/off toggle with a
  three-way dropdown — **Standard**, **Cosy** (beta), and **Agile** (beta).
  The selector sits directly below Battery Mode on the Control page.
- **Agile Octopus tariff integration** (beta):
  - Enter your postcode to auto-detect your Octopus region (via
    postcodes.io), with manual override.
  - Set charge and discharge price thresholds.
  - Live 12×4 price forecast grid colour-coded by action
    (charge / hold / discharge) with summary counts and daily savings
    estimate.
  - Backend state machine polls the Octopus API and automatically
    force-charges or force-discharges based on current price vs thresholds.
  - Reverts to Eco mode when prices sit between thresholds.
  - Uses the same model-aware force charge/discharge commands as Cosy
    (single-phase and three-phase registers supported).
- **Cosy charging now works on three-phase inverters too**: Cosy entry,
  exit, and crash recovery use the correct three-phase registers
  (HR 1123/1122/1112) on compatible inverters.

### Changed

- **Force Charge / Force Discharge are now toggle buttons**: Click once to
  start, click again to stop. The button reflects the current inverter state
  on page load (`enable_charge` + `enable_charge_target` for charge,
  `enable_discharge` for discharge).
- **Pause Battery now matches GivTCP behaviour**: Disables both
  `enable_charge` and `enable_discharge` registers. On three-phase models,
  also clears the three-phase force flags (HR 1122/1123/1112).
- **Status page mode label shows "Override"**: When force charge or force
  discharge is active, the battery mode label on the Energy Flow Diagram and
  Battery page displays "Override" instead of the underlying Eco/Timed label.
- **Pause Battery no longer sets SOC to 100%**: It now just clears charge
  and discharge flags, matching the expected "stop everything" semantics.

### Fixed

- **Force Charge and Force Discharge now work on three-phase inverters**:
  These buttons now check your inverter model and write to the correct
  registers (HR 1123/1122 instead of HR 96/59).

## [0.13.7] - 2026-06-06

### Fixed

- **Changing the refresh rate no longer kicks you off the inverter**:
  Previously, every time you tweaked the polling interval the app would tear
  down the TCP connection and reconnect — because it treated *any* settings
  change as a host/port/serial change. Now the app actually checks what
  changed: if you just adjusted the refresh rate, it keeps the connection
  alive and picks up the new interval within a second. No more pointless
  disconnects for a simple tweak.
- **Debian toolbar icon now matches the app name**: On Debian/GNOME desktops,
  the dock icon was showing a generic placeholder because the desktop file ID
  went out of sync after the rename to Home Energy Manager. The app now
  installs a hidden alias to bridge the gap — your dock icon should actually
  look right now.
- **Tests no longer touch your real settings file**: The settings loader no
  longer auto-creates `~/.givenergy-local/settings.json` as a side effect, and
  tests that need disk I/O now use an isolated temp directory. No more test
  runs accidentally messing with your live config.

## [0.13.6] - 2026-06-06

### Fixed

- **Force-charge, schedule slots, and mode switches actually work again**:
  Every write command sent to the inverter was 36 bytes instead of the
  correct 34 — a double CRC that the dongle silently ignored. Writes timed
  out, nothing happened, but reads still worked so the app looked fine. The
  fix removes the extra CRC, frames are now exactly 34 bytes, and your
  commands actually go through.
- **Cosy Exit no longer traps you in the wrong mode**: When a Cosy charge
  slot finished, the app set `enable_discharge = 1`, which could land your
  inverter in Timed Demand or Timed Export instead of normal Eco
  self-consumption. Cosy exit now properly restores Eco mode every time.
- **Cosy badge now disappears on time**: The "Cosy Charging" badge was
  lingering for one extra poll cycle after a slot ended because the code
  recorded the flag before the state machine ran. Now the badge vanishes
  on the same cycle the slot finishes.
- **Cosy survives a crash**: Cosy state was only kept in memory, so if the
  app crashed mid-slot you'd be stuck in ForceCharge. Now the state is saved
  to disk on every transition. If the app restarts after a crash during a
  slot that has since ended, it fires CosyExit on the very first poll.
- **Tiny daily energy dips no longer trigger false alarms**: The dongle
  sometimes bounces by 0.1 kWh due to 16-bit register precision. The app was
  treating any decrease as register corruption — logging warnings and
  forcing re-polls. Fluctuations under 0.15 kWh are now silently carried
  forward. Material dips still get flagged.

### Added

- **DEBUG logging for write attempts**: The developer console now shows the
  exact frame the app sends — length, MBAP length, and a hex preview — so
  you can confirm writes are the correct size. Handy if we ever break writes
  again.
- **Linux uninstall instructions in the README**: `sudo apt purge
  home-energy-manager`. Your settings and history are kept; delete
  `~/.givenergy-local` separately if you want them gone too.

## [0.13.5] - 2026-06-06

### Changed

- **DMG now has standard /Applications symlink** — restored the normal macOS drag-to-Applications workflow instead of warning users not to use /Applications. The DMG ships with a `Launch.command` script that auto-handles Gatekeeper, zombie cleanup, and quarantine removal on first launch.

- **CI macOS DMG customization updated**: `/Applications` symlink is retained. `Launch.command` is copied into the DMG. README instructions reflect the standard workflow: drag to /Applications, then double-click `Launch.command`.

- **`launch.command` now automates Gatekeeper + zombie cleanup**: copies the app to Desktop if only found in /Applications (macOS 26.5 blocks ad-hoc signed binaries from /Applications), removes quarantine, kills stale 8KB RSS Gatekeeper zombie processes, then launches the app.

- **AGENTS.md documentation updated**: macOS 26.5 known-issues section rewritten to document the standard DMG workflow with one-time "Open Anyway" approval, instead of the previous workaround approach.

### Fixed

- **Tolerate small daily energy decreases from dongle register jitter**: Added
  a tolerance in the delta sanitizer so tiny fluctuations (0.1-0.2 kWh) from
  dongle 16-bit register precision don't trigger false-positive 'register
  corruption' warnings.

## [0.13.4] - 2026-06-05

### Changed

- **Mobile-friendly Quick Actions**: The four Quick Action buttons on the
  Control page now stay on a single row on small screens, with smaller icons
  and tighter spacing on mobile (and normal sizing on tablet/desktop).
- **Larger bottom tab bar icons on mobile**: Bottom navigation icons are now
  significantly larger on phones (with tighter horizontal spacing) so they're
  easier to tap; layout adapts back to the compact form on larger screens.

## [0.13.3] - 2026-06-05

### Added

- **Three-phase charge/discharge schedule editing**: Three-phase, commercial,
  and HV models now read and write their native schedule timer registers
  (HR 1113-1121 for slots 1-2, plus HR 240-299 for slots 3-10). The Control
  page schedule editors are enabled again for these models, with model-aware
  write routing and safe-write whitelist coverage.

### Fixed

- **AC-coupled external CT detection**: AC-coupled inverters now run the
  external CT meter probe after the model-aware re-poll, so systems with
  separate grid/PV CT clamps populate the Meters page reliably.
- **AC-coupled daily solar and consumption totals**: Solar Today no longer uses
  the lifetime PV counter as a daily value. Single-phase/AC-coupled daily solar
  now uses the verified daily generation register and the consumption total is
  computed from the daily energy balance.
- **Three-phase daily solar totals**: Three-phase models now use the verified
  IR 1412-1413 daily PV generation counter instead of the lifetime PV total.
- **Three-phase poll robustness**: Dashboard-critical three-phase input blocks
  are read before optional config/schedule blocks so Status-page power and daily
  energy values are less likely to be starved by optional block timeouts.

## [0.13.2] - 2026-06-05

### Fixed

- **Bizarre solar/load energy history dips** ([#43](https://github.com/psylsph/home-energy-manager/issues/43)):
  `today_solar_kwh` now reads the combined `e_pv_total` at IR(11-12) as a single
  uint32 instead of summing two separate uint16 registers (IR(17)+IR(19)). Each
  of those per-string registers can be independently corrupted by the dongle,
  producing dips in the solar energy chart and amplifying noise in the computed
  consumption formula (`solar + import - export - ac_charge`).
  The three-phase consumption guard now uses a more robust detection (checking
  whether `today_ac_charge_kwh` diverges from `today_consumption_kwh`, which only
  happens when the native `e_load_today` register was decoded).

### Added

- **History grid lines visible in both themes**: The chart grid lines on the
  History page now use theme-aware colors — `#6E7681` in dark mode and
  `#57606A` in light mode — and are thicker (`strokeWidth={2}`) so they're
  easy to see in any theme.

## [0.13.1] - 2026-06-05

### Fixed

- **Three-phase firmware display** ([#48](https://github.com/psylsph/home-energy-manager/issues/48)):
  The Inverter tab now shows the full firmware string from IR 1320-1324 (instead of a
  raw number from IR 1327), plus the DC-side DSP version from IR 1326 — matching
  both GivTCP and the givenergy-modbus reference library.
- **Three-phase CT meter support**: Synthetic grid CT meter is now created from
  IR 1079-1082 import/export power registers, and a second CT meter at IR 1244-1245
  is decoded if present. External meter probe is skipped for three-phase models.
- **Three-phase daily energy totals**: Load energy (`today_consumption_kwh`) is now
  read directly from IR 1396-1397 instead of being derived via formula, and home
  power is read from IR 1089-1090 instead of being formula-derived.

## [0.13.0] - 2026-06-05

### Added

- **Calendar month view for history**: You can now view your history as a
  calendar month — just select "Month" in the time range buttons. It shows
  data from the 1st to the last day of the month, with readings averaged by
  hour. The ◀ Older / Newer ▶ buttons let you page through previous months.

- **Three-phase and HV inverter support**: The dashboard now works properly
  with three-phase, commercial, and high-voltage hybrid inverters. Solar
  generation, grid export/import, battery charge/discharge, and daily energy
  totals now show real values instead of zeros. If you have one of these
  models, you should see live data on the Status page for the first time.

- **Schedule editor hidden for three-phase models**: If your inverter is
  three-phase or HV, the charge/discharge schedule section now shows a notice
  explaining that schedule editing isn't supported yet (these models use a
  different internal register layout). Real-time monitoring and all other
  controls still work.

- **Smarter external meter detection**: The app no longer wastes time probing
  for external CT clamp meters on three-phase inverters — those models have
  their grid CT built in, so the scan is skipped automatically.

### Changed

- **History chart labels are now evenly spaced**: The x-axis labels across all
  time ranges now show clean, evenly-spaced ticks. For example, the 6-hour
  view ticks every hour, the 7-day view ticks every few days, and the month
  view shows day numbers spaced evenly across the calendar. No more odd gaps
  or missing labels.

- **App name consistency**: The browser tab title and error pages now say
  "Home Energy Manager" instead of "givenergy-local".

### Fixed

- **Three-phase home power and consumption**: Home power use and daily
  consumption figures for three-phase models now correctly show the values
  reported by the inverter, rather than being recalculated using a
  single-phase formula that overwrote the real data.

- **Model no longer flips on corrupted register reads**: Once the app
  identifies your inverter model, it locks it in. Previously, a corrupted
  ARM firmware register (HR 21) could flip the displayed model on a single
  bad poll cycle — for example, showing Gen 3 when you have a Gen 2.
  The displayed model is now frozen after the first successful detection
  until the app reconnects.

## [0.12.x] — June 2026

### Added

- **Schedule slot labelling gotchas explained**: Yellow warning banners now
  appear on the schedule page explaining that our slot 1/2 labels are swapped
  vs the GivEnergy Cloud app (the data is the same, only the names differ),
  and that older Gen3 firmware (below version 303) can't use slots 3-10.
  ([#41](https://github.com/psylsph/home-energy-manager/issues/41))
- **AC-coupled battery controls**: Charge/discharge limit sliders now use the
  correct 1-100% range for AC-coupled inverters (was 0-50%).
- **Three-phase and commercial battery controls**: Discharge limits, charge
  limits, SOC reserve, and force charge/discharge now work on three-phase,
  commercial, and high-voltage hybrid inverters.
- **Both firmware versions shown**: Inverter details page now displays ARM
  and DSP firmware versions (helpful for diagnosing partial updates).
- **Smarter inverter detection**: Uses GivEnergy's standard address first,
  then switches to the model-specific one — fewer failed reads on connect.
- **Dongle memory glitch protection**: Recognises corrupted reads from the
  dongle's known memory leak and retries before showing garbage on screen.
- **No more flickering zeros**: When optional data times out, the last known
  values stay on screen instead of flashing to zero for one poll cycle.

### Changed

- **Clearer control labels**: "Battery Limits" → "Battery & Power Limits",
  "Reserve SOC" → "Minimum SOC". Sliders show kW alongside percentages.

### Fixed

- **macOS showing old interface after update**: The app now clears cached
  browser data on startup. If already affected, `Cmd+Shift+R` fixes it.
- **macOS release builds**: Fixed "Resource busy" errors during DMG creation
  on Apple Silicon. Builds now self-verify before uploading.
- **Bottom nav on small phones**: Tabs shrink to icons only on narrow screens.

## [0.11.x] — June 2026

### Added

- **Light/dark mode toggle**: Header theme switch with persisted preference.
- **Roadmap document** so you can see what's planned next.

### Fixed

- **Gen 3 schedule slots 3-10 now actually work** — previously they only
  appeared in the UI but didn't write to the inverter.
- **History charts no longer show sudden mid-day dips**: Stale data is now
  smoothed out — only genuine midnight resets are shown. (#43)
- **Some hybrids showing wrong model**: Gen 1 inverters were being
  misidentified as Gen 3, showing incorrect battery/AC limits. (#40)
- **Mystery charge slots appearing**: A late dongle response mistaken for
  schedule data is now rejected. (#41)

## [0.10.0] — June 2026

### Changed

- **Renamed to Home Energy Manager**: The app is now presented as "Home
  Energy Manager" instead of "GivEnergy-Local". Installer names, start menu
  entries, and browser tab titles all updated. The executable remains
  `givenergy-local` and existing settings/history are preserved.

### Fixed

- **Reserve SOC lower bound**: Consistently enforced at 4% (inverter-safe
  minimum), not 0%.

## [0.9.x] — June 2026

*Heavy development period — lots of new features and fixes.*

### Major features

- **Solar page**: New navigation tab showing PV1 and PV2 input breakdown
  with bar chart and detail cards. PV2 only shown when active.
- **Inverter Details page**: New tab with every available field — model,
  firmware, serial, battery config, rates, modules, and feature status.
- **Human-readable model names**: Status page shows "Gen 3 Hybrid 8kW"
  instead of raw codes. All known GivEnergy models properly identified.
- **Cosy Charging mode**: Software-based charge scheduler — up to 3 slots
  with configurable times and target SOC. Now available to all users (no
  longer hidden behind Developer Mode). Shows "Cosy Charging" badge with
  pulsing green dot when active. Persists across restarts.
- **External CT meter data**: App probes for external clamp meters and
  displays per-phase readings in a new Meters tab.
- **Model-aware polling**: Gen3/AIO inverters automatically read extended
  schedule blocks. AC-coupled and three-phase models read their respective
  config blocks. 10-slot scheduling supported on compatible models.
- **CSV export**: History charts have a CSV button — downloads data with
  ISO timestamps, with native Save As dialog in browsers.
- **Battery calibration** (developer mode): Trigger BMS calibration cycle
  from the Control page.
- **Inverter reboot** (developer mode): Remote reboot button.
- **Multi-instance config**: Set `GIVENERGY_LOCAL_CONFIG_DIR` to run
  multiple copies with separate settings.
- **Linux ARM64 builds**: `.deb` for Raspberry Pi and other ARM Linux
  devices. RPM packages for Fedora/RHEL.
- **State of Health on Battery page**: Shows each module's health %
  (calibrated vs design capacity).
- **Dynamic log level control**: Developer console lets you switch capture
  level to DEBUG for detailed diagnostics.
- **Heartbeat handling**: App now responds to dongle heartbeats, preventing
  automatic disconnection after ~9 minutes.
- **Time-based energy counter sanitization**: Increase threshold scales
  with elapsed time (no more false rejections after a gap).

### Improvements

- **Complete rewrite of mode controls**: Simplified from 4 confusing modes
  (Timed Demand / Timed Export / Export Paused / Eco) to just Eco, Timed
  Discharge, and Paused.
- **Battery mode selector**: Eco/Timed toggle with contextual sub-buttons
  and tooltips explaining what each mode does.
- **Mode change feedback**: Shows "Applying…" message while waiting for
  inverter confirmation.
- **Schedule editors**: 1-minute resolution on time pickers (was 5-min).
  Start/End times side-by-side. Stack vertically on mobile.
- **Chart legends**: Multi-series charts now show colour-coded legends.
  Chart titles are bold and brighter.
- **TCP timeouts increased**: From 5s → 10s → 15s to handle slow dongle
  responses on WiFi bridges.
- **TCP keepalive enabled**: Dead connections detected promptly.
- **ErrorBoundary**: A crash on one page no longer takes down the whole app.
  Shows friendly error with auto-retry countdown.
- **SOC calculation**: Now trusts the inverter's register by default,
  falling back to capacity-weighted BMS aggregate only when needed.

### Schedule & slot fixes

- **No more force-charge when setting a schedule**: Slot settings no longer
  trigger immediate grid force-charge.
- **Slot disable detection fixed**: Uses zero-duration (00:00-00:00) not
  just start=0. Slots at 00:00-08:00 now correctly show as enabled.
- **Charging Cosy**: Now properly restores Eco mode between slots. Survives
  client restarts. Retries failed writes instead of giving up.
- **Gen3 per-slot target SOC**: Individual SOC targets for each schedule
  slot (independent of the global target).
- **Battery power reserve**: New control for minimum discharge power
  reserve (separate from SOC reserve).
- **AC-coupled improvements**: Export priority and EPS enable controls.
  Battery pause mode (HR 318-320). Exception 67 on register 32 handled
  gracefully.
- **Switching inverters no longer fails to reconnect**: Fixed race
  condition in settings version detection.
- **Toggle schedules no longer loses slot settings**: Only the master
  enable flag is written, not the slot times.
- **00:00→00:01 clamping removed**: Now sends 0 for 00:00 matching
  GivTCP exactly.

### Data integrity

- **Consumption always shows 0.0 kWh fixed**: Was reading the wrong
  register (AC charge today, not house consumption). Now computed from
  energy balance: solar + import − export − AC charge. (#30)
- **Import cost spikes fixed**: Three defense gaps closed — cost no longer
  jumps from 17p to £1.75 from a single corrupted reading. (#26)
- **Cost calculation improved**: Midnight rollover detection tightened
  (requires prev > 50 and raw < 10, not just any decrease).
- **History DB repair fixed**: Was corrupting midnight rollover values
  (e.g. 5 kWh → 150 kWh). Now correctly keeps the reset value.
- **Cumulative counter spike suppression removed**: Was blocking
  legitimate large increases after data gaps. MAX aggregation + poll
  sanitizer already handle corruption.
- **Ghost battery modules gone**: Probes validate serial, voltage, and
  capacity before accepting a module.
- **Battery module data disappearing fixed**: Last known-good data
  preserved until a fresh read replaces it.
- **Battery voltage sanitization**: Rejects impossible readings (>60V LV
  or >400V HV).
- **1-minute resolution on schedule timers** (was 5-minute steps).

### UI polish

- **State of Health on Battery page**: Each module shows health %.
- **Schedule slot editors**: Compact card design matching Cosy slots.
- **Schedule hidden during Cosy**: Both charge and discharge schedules
  hidden when cosy is active.
- **Mode display shows "Cosy" everywhere**: Status page, Battery page,
  and mode labels all say "Cosy" when the timer is active.
- **SVG crash on corrupted data fixed**: No more white/blue screen from
  React error #31.
- **ErrorBoundary with auto-retry**: 30-second countdown and "Retry now"
  button on any page crash.
- **Bold axis labels**: History chart ticks now correctly bold.
- **HTTP Port settings fixed**: Save button was invisible (wrong colour
  class), input had white border — both fixed.
- **Battery mode flicker fixed**: Requires 2 consecutive identical
  readings before accepting a mode change.
- **Slider sync**: Sliders re-sync from snapshot on tab switch (no more
  stale values).
- **Cold battery warning**: No longer shows at 0°C before data arrives.
- **Release download guide**: Table explaining which file to download
  for each platform.
- **Configurable HTTP port**: Run multiple instances on different ports.
- **unRAID Docker instructions**: Community-contributed guide.

### Infrastructure

- **Linux ARM64 builds**: `.deb` for Raspberry Pi, etc.
- **RPM packages**: For Fedora/RHEL/openSUSE.
- **macOS DMG build fixes**: Read-only mount, unmatched quote, hdiutil
  auto-appending .dmg extension — all fixed.
- **macOS launch.command**: Bypasses Gatekeeper on macOS 26.5+.

## [0.8.x] — May 2026

*Data sanitisation and cost accuracy focus.*

### Major features

- **Peak/off-peak tariff support**: Separate rates for peak and off-peak
  periods with configurable time windows, for both import and export.
- **Auto-winter persistence**: Original register values saved to disk
  before winter mode activates, restored on restart.
- **History time window alignment**: Charts now align to hour/day
  boundaries for consistent data windows.
- **Multi-layer data sanitization framework**:
  - Absolute range checks on every reading (grid voltage 180-280V,
    frequency 45-55Hz, daily energy 0-200kWh, power ±10kW)
  - Time-based delta checks after 3-reading grace period
  - Monotonic increase enforcement for cumulative daily counters
  - Midnight rollover detection
  - Near-zero previous baseline handling
- **Cost graphs now accurate**: Switched from AVG to MAX aggregation for
  cumulative energy counters. Previously costs were inflated ~1000×.
- **Database repair migration**: Scans and repairs corrupted `today_*_kwh`
  values on startup.
- **Connected clients display**: Settings page shows IPs of all connected
  WebSocket clients.
- **FAQ.md**: Common problems guide covering firewall, LAN access, macOS.
- **launch.command**: Bypasses macOS Gatekeeper on 26.5+.

### Fixed

- **Cost graphs inflated ~1000×**: AVG aggregation of cumulative counters
  understated values; deltas amplified corruption. Now uses MAX.
- **Cumulative counters stuck at corrupted values**: The dongle returns
  garbage on first reads after connect. Now uses 3 warmup reads, resets
  snapshot on reconnect, applies grace period, and always runs absolute
  range checks.
- **Time-based threshold**: Increase limit scales with elapsed time
  (e.g. ~41 kWh after 4-hour gap, not just 2 kWh).
- **Grid voltage/frequency spikes**: 409V and 664V readings caught and
  replaced.
- **Screen flash on disconnect**: Components wrapped with `React.memo`.
- **Missing "Disconnected" broadcast**: Backend now sends Disconnected
  state via WebSocket.
- **macOS Gatekeeper**: App blocked when in /Applications on macOS 26.5+.
  Workaround: move to Desktop. `launch.command` helper provided.
- **LAN access in dev mode**: Axum server serves frontend from `dist/`.
- **Network Access shows LAN IP**: Displays actual LAN IP, not 127.0.0.1.

## [0.7.0] — May 2026

- **Connected clients display**: Settings page shows WebSocket client IPs.
  Local connections labelled "This device".
- **FAQ.md**: Firewall, LAN access, macOS downloads, network scanning.
- **LAN access in dev mode**: Axum server serves frontend from `dist/`.
- **Network Access shows LAN IP**: Real LAN IP instead of 127.0.0.1.

## [0.6.0] — May 2026

- **Developer Mode toggle**: Reveals a Logs page with scrollable,
  filterable backend output. Log capture, text/level filtering, auto-scroll.
- **Network discovery filtering**: Verifies GivEnergy protocol before
  listing a device (no more false positives from other port 8899 services).

## [0.5.x] — May 2026

- **Live snapshot sanitization**: Physically impossible readings
  (battery >10kW, SOC=0 with live power, SOC=100 while charging) are now
  corrected before reaching the frontend, not just filtered from history.
- **BMS SOC validation**: Garbage BMS values rejected. Falls back to
  inverter SOC. Multi-battery SOC uses capacity-weighted average.
- **History recording guards**: Impossible readings no longer written to
  database. Existing garbage entries purged on upgrade.
- **Chart rendering**: Missing data shown as gaps (connectNulls), not
  zero — prevents misleading dips.
- **Battery mode label**: Shows current mode in the energy flow diagram.
- **Schedule slot time pickers**: Stack vertically on mobile (no overlap).
- **Copy URL button**: Works on non-HTTPS LAN contexts.

## [0.5.0] — May 2026

### Added

- **History page**: 5 metric tabs (Battery, Solar, Grid, Home, Cost) with
  time-series charts. SQLite-backed storage. 7 time ranges (1h to 1y).
  Older/Newer navigation. Configurable import/export electricity tariffs.
- **Headless server mode**: Run without a window on Linux
  (`--headless`, `--port`, `--dist`).
- **98 Rust unit tests**.

### Fixed

- **Windows builds**: Frontend now served correctly in installed apps.

## [0.4.0] — May 2026

## [0.3.0] — May 2026

- Non-technical README with download links and quick start guide.
- DESIGN.md with architecture, protocol, and API reference.
- App version shown in Settings → About.
- Energy flow diagram: Home on left, Grid on right.

## [0.2.0] — May 2026

- **Correct Modbus write protocol**: Now uses function code 6 with device
  address 0x11 (was 0x10 with 0x32). Per GivEnergy reference library.
- **Immediate write execution**: Control changes applied as soon as queued,
  not after the next poll cycle.
- **Write-safe whitelist**: Only known-safe registers can be written.
- **Stale frame drain**: Prevents corrupted reads after writes.
- **Faster failure on stubborn registers**: 6 retries with 2s delay
  (previously exponential backoff could block for minutes).
- **No more panic on port bind failure**: Logs error and continues.
- **All CI checks passing**: lint, typecheck, 94 Rust tests.

## [0.1.0] — May 2025

### Added

- Real-time inverter monitoring: solar, battery, grid, home consumption.
- Radial energy flow diagram with live power flows.
- Battery page with per-module breakdown (cell voltages, temps, SOC, cycles).
- Battery mode control: Eco, Timed Demand, Timed Export, Pause.
- Charge/discharge schedule management with time slots and SOC targets.
- SOC reserve, charge rate, and discharge rate controls.
- Auto-discovery of dongle serial from response frame header.
- Network scanner for finding inverters on the LAN.
- WebSocket real-time data streaming.
- Persistent settings (`~/.givenergy-local/settings.json`).
- 94 Rust unit tests.
- Modbus polling resilience, stale response retry, TCP buffer drain.

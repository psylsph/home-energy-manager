# Changelog

All notable changes to this project will be documented in this file.

## [0.38.2] - 2026-06-23

### Fixed

- **Force Discharge with a duration now returns to Eco when the slot ends.** Previously, hitting Force Discharge with a 20-minute Quick Action Duration would leave the inverter in a "paused" state (export mode, discharge enabled, but no active slot — no charge from solar, no discharge) after the slot expired, requiring a manual switch back to Eco. The app now records the slot's end time and the poll loop auto-reverts to the pre-force state once the window closes. Closes [#129](https://github.com/psylsph/home-energy-manager/issues/129).

## [0.38.1] - 2026-06-23

### Added

- **One-click "Submit a Support Bundle" from Settings.** If you raise an issue, the app can now assemble a diagnostic bundle from your current snapshot, the developer log ring, and your alert settings (with secrets stripped) and ship it to the maintainer over ntfy in a couple of clicks — no copying logs by hand. Choose a category, add an optional GitHub issue number for a deep link, write a short description, and hit submit. A 60-second cooldown stops accidental double-submits. Your inverter serial number and LAN host/port are never sent, period; a non-reversible fingerprint of the serial is embedded in the bundle id so the maintainer can correlate repeats from the same install without identifying you. Closes [#125](https://github.com/psylsph/home-energy-manager/issues/125).

- **Read-only API for external consumers like SolarWatch.** A second HTTP server (default port 7338) that serves only `GET /api/snapshot` with Bearer-token auth. Everything you can see in the dashboard — solar generation, battery state, grid import/export, home consumption — is exposed as JSON; nothing else is reachable from this server. Enable it from Settings → Developer: pick a key, pick a port, restart, and your LAN clients can poll the snapshot at `http://<your-hem-ip>:7338/api/snapshot` with `Authorization: Bearer <your-key>`. Addresses [#128](https://github.com/psylsph/home-energy-manager/issues/128).

### Fixed

- **Mobile layout overflows in tariff form, bottom nav, and History tab bar.** The tariff slot editor now reflows to two columns on narrow screens (Start/End on the left, Rate/Remove on the right) instead of overflowing, the bottom nav icons shrink on phones and scroll horizontally when there are too many panels to fit, and the History tab bar switches to a dropdown under the small breakpoint to match the time-range selector.

- **Headless mode on `.rpm` and AppImage installs now serves the web UI automatically.** Previously these installs ran in API-only mode (no dashboard) because the bundled frontend sat at the Tauri resource path (`/usr/lib/givenergy-local/dist/`), which the headless `--dist` search list never checked. The resolver now also looks there, so `.deb`, `.rpm`, and AppImage all serve the UI out of the box. If no `dist/` can be found at any of the search paths, the warning now lists every path it tried plus concrete remediation steps.

- **Modbus reconnects faster on a struggling dongle.** The per-request TCP timeout was 5s and the post-connect warmup was three full reads — both numbers originally chosen to absorb post-reconnect register garbage, which is now handled by the corruption-defence sanitizer. A failing session now reconnects in roughly a third of the time instead of sitting through 30–60s of cumulative timeouts. Closes [#127](https://github.com/psylsph/home-energy-manager/issues/127).

- **Read-only API: server now starts after upgrade and the Settings page keeps your key across restarts.** Two bugs that broke the read-only API on upgrades: existing `settings.json` files written before the feature shipped loaded with `api_port: 0` (because `serde`'s default for `u16` is `0`, not `7338`), so the startup check `api_port > 0` skipped the server even with a saved key; and the Settings page's GET `/api/settings` response didn't include `api_key` or `api_port` at all, so the form came back blank after restart. Both are fixed — existing users now find the API on by default if they had a key saved, and upgrading shows the saved key in the field.

## [0.38.0] - 2026-06-23

### Added

- **Local weather integration with Open-Meteo.** The History page's
  Temperature tab now overlays the actual outside air temperature
  alongside your battery and inverter readings, plus a Battery − Ambient
  differential chart so you can see how much your battery is cooking
  relative to the weather outside. The app fetches the current ambient
  temperature every 15 minutes from the free, keyless Open-Meteo API and
  backfills historical data from the Open-Meteo archive (one calendar
  month per tick, resumable across restarts). A new Local Weather
  section in Settings takes a UK postcode (resolved to coordinates via
  api.postcodes.io) or manual lat/lon for users outside the UK or on a
  self-hosted Open-Meteo instance, with a one-click "Backfill History"
  button that runs in the background and reports progress as it goes.
  Addresses [#121](https://github.com/psylsph/home-energy-manager/issues/121).

### Changed

- **Settings page reordered into a logical flow.** The sections used
  to be numbered inconsistently (two "4"s, a jump from 5 to 7) and the
  grouping was muddled — optional hardware (EV Charger) sat in the
  middle of the getting-started flow, and the data-enrichment sections
  (Tariffs, Weather) were buried after the display settings. The page
  now runs setup-first (Inverter, Remote Access), then how the app runs
  (App), then data sources (Tariffs, Weather, Notifications), then
  display (Panel Controls), with optional and advanced sections
  (EV Charger, Developer) pushed to the bottom. Only the order and the
  section-comment numbers changed — no content was rewritten.

## [0.37.4] - 2026-06-23

### Added

- **Read-only URL (`?RO`) for sharing the dashboard with the family.**
  The Settings page → Remote / Mobile Network Access section now
  shows a read-only version of the LAN URL with `?RO` appended.
  Visiting the URL with that flag hides the Control and Settings tabs
  from the bottom navigation in the visitor's browser, and the flag
  is pinned in localStorage so it stays hidden across reloads. The
  owner's browser is unaffected — each browser keeps its own setting.
  Intended for a household-shared dashboard link (e.g. for a partner
  or kids) where the goal is to prevent accidental changes, not
  security. Display-only, trust-based — no server-side enforcement.
  Closes
  [#114](https://github.com/psylsph/home-energy-manager/issues/114).

## [0.37.3] - 2026-06-23

### Fixed

- **Sanitizer false-positive spam on common transitions.** Five
  patterns the corruption-defence layer was wrongly flagging: a
  poisoned grace-period baseline making daily counters log
  "jumped too fast" for ~30 s after every reconnect; battery waking
  up from idle (0 W → 3 kW) tripping the rate-check fraction gate
  against a zero base; single-phase consumption wobble (5 ticks of
  derived-counter drift) flagged as corruption; the 99 → 100 SOC
  rounding tick while charging being rejected; and the rate check
  re-spamming every 3 s while the inverter consistently reported a
  new steady state. A consecutive-correction release now settles
  sustained transitions after three cycles, and the SOC and daily-
  counter carries-forward are gated on `prev` being meaningfully away
  from the boundary so legitimate rounding ticks are accepted.

### Changed

- **Downgraded a handful of `WARN` logs to `INFO`** for sanitizer
  cases that turn out to be false positives in practice (rate-check
  jumps, daily/lifetime energy decreased/jumped-too-fast, and the
  soft-over-limit "out of range"). Genuine corruption signals —
  int16 saturation, absolute-range violations, slot-time corruption,
  and the hard limits on grid voltage / battery temperature —
  stay at `WARN`.

## [0.37.2] - 2026-06-22

### Added

- **Connection-lost notifications.** Settings → Alerts now has a
  "Connection Lost" toggle. When enabled, you'll get a notification
  through the same channel you already have set up (Telegram, ntfy,
  or Pushover) the moment the app drops contact with the inverter,
  plus a follow-up when it comes back. Fits well with the
  push-notification setup for battery warnings — if the dongle dies
  overnight, you'll know before the morning kettle. Closes
  [#115](https://github.com/psylsph/home-energy-manager/issues/115).

## [0.37.1] - 2026-06-22

### Added

- **Longer refresh interval options.** Settings → App now offers 30 s,
  45 s, and 60 s alongside the existing 5/10/15/20 s buttons. Helpful
  for users on borderline dongles that wedge under a tight poll loop
  (the Modbus application processor is more forgiving at slower
  cadences, and the in-loop zombie-dongle watchdog has a longer grace
  window to ride out transient slow responses).

### Fixed

- **Tariff slot editor was over-restrictive.** Adding a new window
  locked both the new row's start and end times: the start `<select>`
  was disabled for all non-first slots, and the end `<select>` was
  capped at the next slot's start. You can now edit a non-first slot's
  start directly (constrained between the previous slot's end and this
  slot's end), and an intermediate slot's end is freely selectable up to
  `23:59`. Changing a start cascades backward to the previous slot's
  end, mirroring the existing forward cascade for end changes — the
  day stays tiled automatically.

- **Battery energy didn't reset at midnight for Today / 24h.** The
  inverter's `today_*_kwh` counters reset at UTC midnight, but the
  history repair compared timestamps using local-day boundaries. In
  timezones east of UTC (e.g. BST, UTC+1) a reading at 23:30 UTC falls
  on the next local day, so the repair saw the 00:30 UTC reset as a
  "same-day decrease" and carried yesterday's final counter value into
  today. The repair now compares UTC dates. The History page chart
  also shifts fetched timestamps from UTC to local time so the reset
  appears at 00:00 on the local-time X-axis; readings from 00:00–01:00
  that still carry yesterday's values are trimmed from the Today view.

## [0.37.0] - 2026-06-22

### Added

- **Multi-window time-of-use tariffs.** The flat peak/off-peak rate is
  gone. You can now define up to six rate windows for import and export
  separately, which unlocks Octopus Flux, Cosy, Agile, and similar
  tariffs. The Settings page has a slot editor; existing two-rate
  settings.json files migrate transparently on first load. History cost
  charts, the daily HTML report, and the Telegram `/today` summary all
  pick up the new rate automatically.

- **"Start on Login" toggle.** Settings → App now has a switch that
  registers the app to launch when you log in, matching whatever your
  OS's "Startup apps" panel would do (registry entry on Windows,
  LaunchAgent on macOS, `.desktop` file on Linux). Headless / Pi installs
  hide it because the systemd service is the equivalent there. Fixes
  [#117](https://github.com/psylsph/home-energy-manager/issues/117).

- **Temperature tab on the History page.** New chart tab alongside
  Battery, Solar, Grid, Home and Cost. Plots inverter and battery
  temperatures and a derived `Battery − Inverter` differential so you
  can see how the two thermal paths drift apart through a charge cycle.

### Improved

- **"Today" tiles show the full energy balance.** The Status page used
  to display a single "Consumption" figure, which silently hid today's
  net battery charge. The tile grid is now six cells — Solar, Home Use,
  Battery Charged, Battery Discharged, Import, Export — that balance:
  `home_use + charge − discharge ≈ solar + import − export`. Each tile
  has a tooltip explaining what it includes, and the energy-balance
  formula is shown under the grid. Closes #122.

- **Control page section renamed.** "Battery & Power Limits" → "Battery
  and Power Controls". The `&` no longer breaks up the sentence.

- **Control tab is hideable.** Settings → Panel Controls → Panel
  Visibility now lists the Control tab alongside the other bottom-nav
  tabs, so users who don't use manual override can hide it. Fixes #120.

- **Tariff slots use real clock times.** The final slot's `end` is now
  `"23:59"` instead of the sentinel `"24:00"` (which isn't a real time
  and required special-case handling everywhere). Old `"24:00"` shapes
  in settings.json migrate transparently on read.

### Fixed

- **External links open in your default browser.** Settings → About and
  the Telegram / ntfy / Pushover help URLs were opening inside the
  WebView2 process on packaged builds because `tauri-plugin-opener` was
  only initialised in debug mode. The plugin is now registered
  unconditionally and the About link is a button that calls the existing
  `openExternal` helper. Fixes
  [#118](https://github.com/psylsph/home-energy-manager/issues/118).

- **`POST /api/reconnect` resets the zombie-dongle back-off.** If you
  clicked Reconnect while the poll loop was deep in a long
  zombie-dongle back-off sleep (up to 10 minutes), it only triggered
  one extra poll before falling back asleep. The reconnect handler now
  also bumps the outer back-off counter so the next retry happens
  immediately.

- **Power-balance cross-check stops false alarms on legitimate fast
  transitions.** The rate-based smoother (added in 0.36.3) was correctly
  rejecting dongle corruption spikes but was also catching fast but
  physically valid transitions — most noticeably an EV charger ramping
  in over a single three-second poll, which would log a spurious
  `home_power jumped too far` warning and hold the home-load figure at
  the previous value for one cycle. The new cross-check runs after the
  per-field absolute and rate checks and uses the energy-balance
  identity `home = solar + battery − grid` to identify the field that's
  actually inconsistent with the other three. When exactly one field
  disagrees with the rest, that field's rate sanitisation is undone and
  the raw reading is restored. It can only undo a rate sanitisation
  (never add a new one), it's gated on a 2 kW residual and 1 kW
  per-field movement so genuine corruption where all four fields
  disagree together is left alone, and it's skipped during the
  post-connect grace period and on gateway/EMS installs where
  `grid_power` is derived from the same identity.

## [0.36.3] - 2026-06-22

### Improved

- **Better protection against sudden power spikes from the data adapter.**
  The app now catches readings that look plausible on their own but jump
  unrealistically fast between updates — for example solar power leaping
  from 2 kW to 7 kW in a single three-second poll. Both values are within
  the normal range, but such a rapid swing is physically impossible and
  was passing through the existing checks. The new rate-based filter holds
  the previous reading for one cycle, then lets the real value through
  once it catches up. This mirrors the multi-layer corruption defence used
  by GivTCP.

## [0.36.2] - 2026-06-21

### Fixed

- **Three-phase battery power derating setting now read correctly.** The app
  was polling the register but silently discarding it, so the battery power
  cutoff percentage never appeared on three-phase models. It's now decoded
  properly.

- **Three-phase battery maintenance mode now read.** The register was in the
  polled range but wasn't being decoded, so maintenance mode (off / discharge /
  charge / standby) was invisible on three-phase models.

- **Battery secondary capacity register now decoded.** The app now reads the
  second design capacity value from the battery management system, which can
  help detect calibration drift between the two capacity registers.

- **Meter neutral-line current now read.** External CT clamp meters report
  neutral-line current on a dedicated register that the app was skipping. It's
  now decoded alongside the phase currents.

## [0.36.1] - 2026-06-21

### Fixed

- **All-in-One charge power limit reverting to maximum.** On All-in-One
  inverters, setting a lower charge power limit (e.g. 3 kW) would appear to
  work briefly, then silently jump back to the full 6 kW. The app was
  mistakenly reading the charge limit from a register that only applies to
  AC-coupled inverters; the All-in-One uses a different register that was
  being overwritten. Charge and discharge power limits now stay where you set
  them on All-in-One models.

- **Scheduled charge target jumping back to 4%.** When configuring a charge
  schedule slot with a specific target state of charge, the target would
  sometimes snap back to 4% on the next poll. The inverter reports "no target
  set" as a zero in the per-slot register, which the app was treating as a
  valid minimum value (4%) instead of falling back to the global target. Slot
  targets now fall back correctly to the global charge target when the
  inverter hasn't echoed a per-slot value.

- **All-in-One charge schedule not honouring the target SOC.** Configuring a
  charge slot with a target below 100% on an All-in-One inverter would charge
  to 100% anyway — the app wasn't writing the global target register that the
  All-in-One keys off. The global target is now written alongside the
  per-slot value so the charge stops at your chosen level.

### Added

- **Comprehensive register audit documentation.** Added a detailed comparison
  of how every inverter, battery, and meter register is interpreted across the
  app, GivTCP, and the community reference library. This internal reference
  documents every known difference and confirms the sign conventions, scaling,
  and register addresses are correct.

## [0.36.0] - 2026-06-21

### Added

- **Pushover** as a third notification channel alongside Telegram and ntfy.
  Pushover is a paid-once-per-platform push notification service that
  requires two credentials — an App API Token (register your own application
  at <https://pushover.net/apps/build>, per Pushover's guidance for
  distributed OSS apps) and your account's User Key. Alerts and all-clear
  notifications fan out to all three channels in parallel; the daily
  consumption report remains Telegram-only for now. Closes #101.

## [0.35.0] - 2026-06-21

### Added

- **Force Charge / Discharge Duration control** under Battery and Power
  Controls. A slider (1–1440 minutes, default 30) sets how long the Quick
  Action buttons should force the inverter to run. Persisted to
  localStorage. Matches GivTCP's behaviour where the user picks a
  duration and the inverter auto-reverts when the slot ends.

- **Stop Force Charge** button now restores the inverter to its pre-force-charge
  state instead of switching to Eco mode (which left the user's discharge
  schedule silently disabled and a Max-Power user stuck in Eco). The
  pre-state is captured on Force Charge start and replayed on Stop, mirroring
  GivTCP's `forceCharge`/`FCResume`. Restores `HR_ENABLE_CHARGE`,
  `HR_ENABLE_CHARGE_TARGET`, `HR_ENABLE_DISCHARGE`, `HR_CHARGE_TARGET_SOC`,
  `HR_CHARGE_SLOT_1_START/END`, and `HR_BATTERY_POWER_MODE` to their prior
  values. Three-phase models also restore `HR_3PH_FORCE_CHARGE_ENABLE` and
  `HR_3PH_AC_CHARGE_ENABLE`.

- **Stop Force Discharge** button now restores the inverter to its
  pre-force-discharge state, mirroring GivTCP's `forceExport`/`FEResume`.
  Restores both discharge slots, the discharge rate, the enable flags, and
  the battery power mode.

- **`POST /api/control/force-charge/stop`** and
  **`POST /api/control/force-discharge/stop`** endpoints. Both return 400
  if no force operation is in progress (defensive — won't clobber a
  working schedule if the user double-clicks).

- **Force Discharge now honours the duration slider.** Previously the
  Quick Action always wrote a 00:00–23:59 discharge slot. Now the
  discharge slot is `now → now+minutes` (or 3PH equivalent), matching
  GivTCP's `forceExport` `set_mode_storage(discharge_slot_1=TimeSlot{now, now+exportTime})`.
  The no-body path still produces a full-day slot for backward
  compatibility with any existing callers.

- **`battery_power_mode` on `InverterSnapshot`** (HR 27 raw value).
  Required to restore the user's pre-force-charge power mode (0 = export,
  1 = eco) on Stop. Default 1 (eco) on uninitialised snapshots for safety.

### Tests

- Added 17 vitest tests for the `forceDuration` helpers (label
  formatting, slider clamping, localStorage round-trip).
- Added 16 Playwright mock-based E2E tests in `e2e/force-stop.spec.ts`
  verifying the exact Modbus register writes produced by Force
  Charge/Discharge start and stop, including the silent-disable
  regression for `HR_ENABLE_DISCHARGE` and the slot-restoration
  ordering.
- Added 8 Playwright simulator-based E2E tests in
  `e2e/local-force-stop.spec.ts` and 8 in `e2e/local-force-duration.spec.ts`
  exercising the start→stop flow against the real GivEnergy simulator
  (full Modbus TCP stack on port 18899).
- Added a `Force Charge` / `Force Discharge` revert-independence
  Rust test (22 force-related Rust tests in total, all passing).

## [0.34.3] - 2026-06-21

### Fixed

- **Gateway battery power direction is now correct.** On Gateway (parallel
  AIO) installs the battery flow arrow, the `battery_power` value, the
  derived grid power and the per-AIO power figures were all inverted
  relative to real hardware. A user on v0.34.2 reported the classic symptom
  (issue #78): solar 6.3 kW and home 0.6 kW, yet the app showed the battery
  *discharging* at −5.5 kW with 11.2 kW grid export — physically impossible,
  since export cannot exceed solar + battery discharge.

  **Root cause:** the GivEnergy gateway's `p_aio_total` register (IR 1702)
  and the per-AIO `p_aioN_inverter` registers (IR 1816–1818) use the
  **opposite** wire sign to a standard inverter's `p_battery` (IR 52): raw
  **+ = charging**, − = discharging. GivTCP confirms this by negating the
  value (`GivTCP/read.py:1556`: `Battery_Power = -GEInv.p_aio_total`). The
  v0.32.0 sign-convention change mapped these verbatim on the (incorrect)
  assumption that the gateway shared IR(52)'s + = discharge convention. The
  decoder now **negates** `p_aio_total` and the per-AIO registers on decode so
  HEM's internal + = discharge convention matches the rest of the app. This
  also makes the derived grid-power balance (`solar + battery − home`)
  produce sensible figures again. (Issue #78.)

## [0.34.2] - 2026-06-20

### Fixed

- **Total Throughput and Battery Life Remaining rows on the Battery page
  are now hidden on AC-coupled and All-in-One inverters.** GivEnergy
  firmware does not populate any lifetime throughput register on these
  models, so the v0.34.0 behaviour of showing a dash for an empty meter
  was misleading — it suggested a bug rather than a missing firmware
  feature. The rows now hide entirely when the meter is empty, matching
  the behaviour on a fresh hybrid install before any charge cycle has
  completed.

## [0.34.1] - 2026-06-20

### Added

- **Inverter age / operating hours on the Inverter page.** The page now
  shows how long the inverter has been running, decoded from the
  inverter's internal work-time counter and rendered as a friendly age
  like "3y 4m". The exact hour count is shown next to it (for example
  "29 400 h") so you can see the raw figure too. The row is hidden on
  inverters that haven't reported a value yet.

### Changed

- **Battery Mode on the Inverter page is now shown as Upper Camel Case**
  (for example `EcoPaused`, `TimedExport`) instead of the raw
  `eco_paused` / `timed_export` enum values from the backend. Other
  pages that display battery mode keep their existing labels.

## [0.34.0] - 2026-06-20

### Added

- **Emergency Power Supply (EPS) toggle on the Control page.** Owners of
  AC-coupled, AC three-phase, and All-in-One inverters can now turn EPS
  (backup power during grid outages) on and off from the app. Previously
  the inverter could do this from its own display, but the app had no way
  to control it.

- **EPS power shown on the Battery panel.** While EPS is actively feeding
  the backup loads during a grid outage, the Status and Battery pages now
  display the live EPS power below the main battery Power reading. The
  row appears only when EPS is doing real work, so it stays hidden during
  normal grid-connected operation and on inverters that don't support EPS.

### Changed

- **EPS and battery lifetime rows use a dash for missing data instead of
  hiding.** The "Total Throughput" and "Battery Life Remaining" rows on
  the Battery page now show a dash when the inverter hasn't reported a
  value yet, rather than disappearing entirely. This makes it clearer
  that the meter is empty rather than the battery having done no work.

### Fixed

- **EPS toggle is only shown on inverters that support it.** DC hybrids
  and pure three-phase inverters don't have an EPS output stage, so the
  firmware silently ignored HR 317 writes and the toggle did nothing.
  The toggle is now hidden on those models, and the backend API returns
  a clear error if the endpoint is hit manually. EPS remains visible for
  AC-coupled, AC three-phase, and All-in-One inverters.

## [0.33.2] - 2026-06-20

### Fixed

- **Gen1 Hybrid daily battery energy now uses the alternative registers.**
  Some Gen1 firmware populates `IR(36)/IR(37)` with 0 while the real daily
  battery charge/discharge values live in `IR(183)/IR(182)` (the reference
  library's `alt2` source). The single-phase poll block now reads
  `IR(180-183)`, and the decoder routes daily battery energy by device type:
  Gen1 Hybrid uses the alt2 registers, while all other single-phase models
  continue to use `IR(36)/IR(37)`. `today_consumption_kwh` is recomputed from
  the routed values so the dashboard consumption figure is also correct on
  affected Gen1 systems.

## [0.33.1] - 2026-06-20

### Fixed

- "Send Test Notification" button now activates when only ntfy is configured (no Telegram required)

## [0.33.0] - 2026-06-20

### Added

- **Persistent rotating log files.** App output is now written to a
  daily-rotated file at `~/.givenergy-local/logs/app.log.YYYY-MM-DD`
  (`%USERPROFILE%\.givenergy-local\logs\` on Windows) in addition to the
  existing in-memory ring buffer shown in the developer console. Previously
  logs were kept only in memory and lost on every restart, with developer mode
  merely surfacing that ephemeral buffer. Each day produces one bounded file,
  and files older than 14 days are pruned on startup so total disk usage stays
  capped. The file layer defaults to INFO level (more verbose than the terse
  WARN console/ring streams) and runs independently of developer mode.

## [0.32.1] - 2026-06-20

### Fixed

- **ntfy push notification configuration is now always visible when alerts
  are enabled.** Previously the ntfy topic field, Copy button, and server
  field were hidden behind an inverter-serial check, so users who enabled
  alerts before connecting to an inverter (or with a non-reporting device)
  saw only a "Connect to an inverter to generate your ntfy topic" message and
  had no way to configure ntfy. The topic is now an always-visible editable
  field that auto-fills from the inverter serial when connected and can be
  set manually at any time. Also removed the silent auto-save `useEffect`
  that could clobber a manually-entered topic.

### Changed

- Updated the Windows security notices in `README.md`, `INSTALL.md`, and the
  release workflow. The installer is scanned clean by VirusTotal; the old
  `Trojan:Script/Wacatac.C!ml` false-positive wording and the SignPath
  Foundation code-signing plan (application rejected) have been removed, and
  users are now asked to report any antivirus warning as a security
  vulnerability via the GitHub issues page.

### Docs

- Fixed all markdown lint errors across the documentation (`PROBLEM.md`,
  `ROADMAP.md`, `INSTALL.md`, `README.md`, and the `gateway-design/` notes) so
  `npm run lint:md` passes clean.

## [0.32.0] - 2026-06-20

### Fixed

- **Battery power sign convention now matches the GivEnergy app and the
  givenergy-modbus / GivTCP reference libraries.** `battery_power` is now
  **positive when discharging** and negative when charging (previously the
  opposite). Raw register values from IR(52) (single-phase), IR(1136-1139)
  (three-phase) and IR(1702) (`p_aio_total`, Gateway) are mapped verbatim
  onto `battery_power` instead of being negated. Internal derivation
  formulas, `BatteryState`, frontend display signs, history charting and the
  Power page integration have been flipped consistently. This fixes the
  inverted battery power reported by Gateway users (issue #78); display
  values for already-working inverters are unchanged.

- **Single-phase home consumption is now read directly from the inverter
  instead of being derived.** `home_power` now uses IR(42) `p_load_demand`
  (the inverter's independently-sensed busbar load) as its primary source,
  falling back to the `solar + battery − grid` balance only when that
  register returns 0. The derived identity disagreed with the measured load
  in roughly two-thirds of poll cycles (a real conversion-loss residual), so
  the Status page's radial energy-flow diagram now agrees with GivTCP and the
  GivEnergy app. This brings single-phase in line with the three-phase
  (IR 1089-1090 `p_load_all`) and Gateway (IR 1618 `p_load`) paths, which
  already read authoritative load registers.

- **"PV Energy Today" no longer over-reports on single-phase inverters.**
  The daily solar total now prefers the per-string registers IR(17)/IR(19)
  over IR(44), giving a value 5–10% lower that matches the GivEnergy app and
  other third-party monitors. A one-time history-database repair also
  reconstructs corrupted or stuck cumulative `today_solar_kwh` rows from the
  recorded `solar_power` samples; the database is backed up first and the
  repair runs only once (gated by a flag in the `meta` table).

- **Spurious PV2 readings on single-string inverters are suppressed.** When
  no second string is fitted, PV2 voltage and current are both zero and the
  PV2 power field is now forced to zero so leftover register garbage cannot
  inflate the solar total.

- **Export Paused mode now actually pauses discharge.** Engaging the
  "Paused" battery mode now writes `enable_discharge = 0` and clears the
  discharge slots, instead of leaving discharge running.

- **Alert configuration is now restored on startup.** Persisted alert
  thresholds and notification settings are applied to the live alert engine
  during startup, so alerts fire immediately after launch instead of only
  after the settings were next saved.

### Added

- **Dedicated `home_energy_today_kwh` field** for home consumption energy.
  The Status page "Consumption" tile and the History "Load Energy Today"
  chart now use this field. A one-time database backfill copies legacy
  `today_consumption_kwh` values across so existing historic chart data is
  preserved. The conditional "(excl. EV)" suffix has been dropped from the
  Consumption tile label.

- **Docker "Today" chart no longer starts at 01:00 during BST.** In
  headless/Docker mode the history "Today" window is computed server-side
  with `chrono::Local`, but the `debian:bookworm-slim` image has no timezone
  data, so it fell back to UTC and the day boundary landed at 01:00 local
  time in zones east of UTC (e.g. BST). The image now ships `tzdata`, and
  `docker-compose.yml` mounts the host's `/etc/localtime` read-only so the
  container inherits the host's timezone without needing `TZ` set at all.
  An optional `TZ` override (commented in the compose file) takes precedence
  when the host has no `/etc/localtime` or a different zone is desired.

### Changed

- History charts with kWh units now show one decimal place on the Y-axis and
  tooltips instead of rounding to a whole number.

### Tests

- Added dedicated sign-convention tests covering every code path that
  participates in the battery-power convention (single-phase, three-phase,
  Gateway, derived home/grid formulas, invalid-version guard), plus tests
  for the new IR(42) home-power source and its zero-value fallback.

## [0.31.8] - 2026-06-19

### Fixed

- **Telegram poller no longer reuses stale pooled connections** (fixes the
  recurring `Telegram poll error: timeout: global` logged roughly every 5
  minutes on NAT'd / containerised networks). The Bot API agent now pins
  `max_idle_connections` and `max_idle_connections_per_host` to `0`, so every
  poll opens a fresh TCP+TLS connection that is too young to have been reaped
  by a middlebox's idle-state garbage collector. (TCP keepalive would have
  been the more efficient fix, but ureq 3.x keeps its `TcpTransport` in a
  private module and exposes no public socket/transport hook, so it isn't
  reachable from application code without re-implementing the whole HTTP/1.1
  transport.)

- **Exponential backoff on repeated Telegram poll failures.** The poller now
  backs off `3 → 6 → 12 → 24 → 48 → 60s` (capped, then held) on consecutive
  transport failures, resetting to the 3s base cadence on the first success.
  This damps log spam and request rate during a sustained outage (revoked
  token, broken route, …) while still probing for recovery. The warning line
  now also reports the consecutive-failure count and the next-attempt delay.

- **Poll timeouts demoted from WARN to INFO.** A `getUpdates` timeout is now
  treated as benign — the Bot API's long-poll is *designed* to hold a
  connection open waiting for updates, so a timeout just means "no updates
  this cycle, retry," and is fully handled by the backoff. Timeouts are logged
  at INFO (below the default WARN console level, so the recurring
  `Telegram poll error: timeout: global` lines disappear from the dev console
  and stdout unless you bump logging to INFO). Genuine non-timeout failures
  (DNS, connection refused, an expired bot token returning HTTP 401, …) still
  log at WARN since they may need attention. Both severities still feed the
  backoff counter.

- **Frozen daily energy counters when the grace-period baseline was corrupted
  low.** The sanitizer's "jumped too fast" delta branch (both daily
  `today_*_kwh` and lifetime `total_*_kwh` counters) rejected an implausibly
  large jump and carried the previous value forward — but had no recovery
  path, so a single corrupted *low* grace-period baseline poisoned the field
  forever. Every subsequent real (higher) reading was also "too fast"
  relative to the stuck baseline, freezing the counter — e.g. on AC-coupled
  inverters `today_export_kwh` stuck at ~1.0 kWh while the inverter reported
  18.5, spamming `Daily energy jumped too fast - using previous` every poll
  cycle. Symptom: the Solar history "PV Energy Today (kWh)" flat-lined and
  the derived "Solar Today" / "Home Consumption" values were wrong, while
  instantaneous PV Power (W) looked fine. The branch now mirrors the existing
  decrease-branch release: after 10 consecutive jumps to the same raw value
  the baseline is accepted as the corrupted reading and the raw value is
  taken (logged at INFO), with the warning downgraded to DEBUG after 3 cycles
  while recovering. A genuine single transient spike is still rejected.

- **`ErrorBoundary` auto-retry now starts for errors present at mount.**
  `componentDidUpdate` never fires when a child throws on the initial render,
  so a page that threw on load showed "Will retry in 30s" but the countdown
  never ticked (only the manual "Retry now" button worked). The countdown is
  now also started in `componentDidMount`. Every route is also now wrapped
  structurally via a single `page()` helper, so a route can no longer be
  added without an `ErrorBoundary`.

### Changed

- **History charts no longer invent non-monotonic values when repairing
  cumulative-counter spikes.** `removeSpikes()` now carries the previous good
  value forward for cumulative daily counters (`today_*_kwh`) instead of
  interpolating the midpoint of their neighbours — a synthetic value that
  broke monotonicity and corrupted per-bar kWh deltas and derived cost.
  Instantaneous rates and gauges (power, voltage, SOC) still interpolate,
  where a midpoint is the least-bad estimate.

- **Single-phase poll trims 58 unused registers per cycle.** The `input_180`
  block now reads only IR 180/181 (the two alternative battery lifetime
  totals the decoder consumes), not the full 60-register window — IR 182-239
  are absent from the authoritative register map for every model.

## [0.31.7] - 2026-06-19

### Added

- **Notification setup guide.** New `NOTIFICATIONS.md` with step-by-step
  instructions for Telegram and ntfy, from creating a bot to receiving the
  first alert.
- **FAQ entry.** *"How do I set up Telegram or ntfy notifications?"* links
  to the new guide.
- **Settings page link.** A "Setup guide ↗" button next to the Notifications
  section opens the guide directly.

## [0.31.6] - 2026-06-19

### Fixed

- **Raspberry Pi `.deb` package now includes the frontend `dist/` directory.**
  The headless binary (`givenergy-local --headless`) on ARM `.deb` installs was
  running in API-only mode because the Vite-built frontend wasn't bundled at a
  path the binary searched. `tauri.conf.json` now uses `deb.files` to place
  `dist/` at `/usr/share/givenergy-local/dist/`, which matches the Docker
  convention and `resolve_dist_dir()`'s search order.

### Changed

- **Lint fixes:** `useWebSocket.ts` window-assignment wrapped in `useEffect`;
  `SettingsPage.tsx` added missing `useEffect` deps (`url`, `useRef` for ntfy
  topic guard).

## [0.31.5] - 2026-06-18

### Fixed

- **macOS minimum version now explicitly set to 10.15 (Catalina).** The app
  bundle now declares `minimumSystemVersion: "10.15"` so macOS shows a clear
  error dialog on older systems instead of a blank white screen. The blank
  screen was caused by modern JavaScript syntax (optional chaining, nullish
  coalescing, etc.) that Safari 12 on macOS 10.14 (Mojave) cannot parse.

## [0.31.4] - 2026-06-18

### Added

- **Telegram `/report` command.** Send `/report` in your Telegram chat to
  receive yesterday's full consumption report as an HTML document (tap to open
  — includes charts, hourly breakdowns, totals). The caption shows the same
  plain-text summary as a `/today` reply.

### Changed

- **Daily consumption report now actually delivers.** The HTML report was
  previously generated at the configured time but silently discarded — the
  underscore in `let Some(_report_body)` meant it was never sent to Telegram.
  It now sends the report as a document attachment via `sendDocument` with the
  plain-text summary as the caption.
- **Daily summary no longer shows misleading `0.0 kWh off-peak`** when no
  structured tariff (with off-peak window) is configured. The peak/off-peak
  breakdown line is hidden unless the off-peak window is set in Settings →
  Tariff.

## [0.31.3] - 2026-06-18

### Removed

- **WhatsApp alert channel.** The experimental WhatsApp (QR-pairing) alert
  delivery has been removed. It never worked reliably: a freshly-linked
  companion device cannot establish Signal Protocol sessions with the other
  devices of the *same* account it was paired to, so alert messages were
  accepted by WhatsApp's server but silently dropped ("session … not found",
  only `sender` receipts, never `delivered`). This is a limitation of the
  unofficial WhatsApp Web protocol (the same affects Baileys/whatsmeow), not a
  bug we could fix in app code. **Telegram and ntfy remain the recommended,
  reliable alert channels** — ntfy in particular is free, needs no second phone
  number, and delivers push notifications to your lock screen. The
  `whatsapp-rust`/`wacore` dependencies and the custom `whatsapp-store.db`
  backend were removed along with the `/api/whatsapp/*` endpoints and the
  Settings → WhatsApp pairing UI. The project **no longer requires a Rust
  nightly toolchain** (stable is now used in CI).

### Added

- **Solar Clipping alert.** A new alert fires when solar generation is
  sustained above a user-configured manual ceiling, indicating the inverter is
  likely curtailing output. Set the ceiling (in watts) under Settings → Alert
  Triggers & Cooldown → Solar Clipping; set to your inverter's rated AC output.
  Like the hardware battery warning, it requires **3 consecutive cycles** of
  agreement before firing, so a momentary cloud-edge spike above the ceiling
  does not trigger it. Disabled by default (ceiling 0). The ceiling is a
  manual value because the inverter's own limit is not always reliably
  available, and for external-CT-sourced PV there is no nameplate at all.

### Changed

- **Settings → Alert Triggers layout tidy-ups.** The "Cooldown" field is now
  a "Cooldown Timer" heading with the value entry below it; the cooldown and
  solar-clipping value fields are now aligned and equal-width; the
  "Battery temperature alerts only work with inverters that report
  temperature…" notice moved to sit as a sub-heading under "Battery
  Temperature & SOC" where it belongs; the About link now points to
  <https://psylsph.github.io/home-energy-manager/>.

## [0.31.2] - 2026-06-18

### Fixed

- **Telegram bot commands delayed by minutes, or delivered in a burst.** The
  Telegram poller runs a single sequential loop (`sleep → getUpdates → reply`)
  and the `getUpdates` call used the default ureq client with **no client-side
  timeout**. The `timeout=10` query parameter only governs the *server-side*
  long-poll, not the client read. When the held-open connection stalled at the
  network layer (common in containerised/Docker deployments via DNS or MTU
  quirks), nothing could rescue it — the poller froze for the OS-level TCP
  timeout (minutes), while every command sent during the stall queued at
  Telegram and then arrived in a single scrambled burst once the connection
  recovered.

  All Telegram Bot API HTTP calls now go through a shared `ureq::Agent`
  configured with a 20s end-to-end timeout (`timeout_global`). A stalled call
  now dies after 20s and the poll loop continues immediately, so command
  latency is bounded (~23s worst case) instead of minutes. The 20s limit
  comfortably exceeds the 10s server-side long-poll, so behaviour is unchanged
  when the network is healthy. The shared agent also pools connections,
  removing the per-call TLS handshake overhead.

## [0.31.1] - 2026-06-18

### Fixed

- **Spurious "Battery Over-Temperature" alert on transient reads** — the alert
  fired from a single corrupted read of the inverter's hardware warning
  register (IR 57), which is not otherwise sanitised. This caused reports like
  an over-temp warning firing at 21.5°C with the threshold set to 45°C: the
  alert wasn't gated by the °C setting at all. The flag now requires
  3 consecutive cycles of agreement before firing, so a one-off garbage read
  can't trigger it.

### Changed

- **Renamed the alert to "Inverter Battery Warning"** — it was confusingly
  labelled "Battery Over-Temperature", which users conflated with the °C-based
  "Battery Temperature High" alert (the one the 45/50°C threshold actually
  controls). The renamed alert is the inverter/BMS's own hardware warning flag
  and is intentionally independent of the configured temperature limit, so a
  genuine device warning always gets through.

## [0.31.0] - 2026-06-18

### Added

- **More Telegram bot commands** — alongside `/status`, the bot now responds to:
  - `/today` — today's energy summary with a peak/off-peak cost split derived
    from your configured tariffs and net cost.
  - `/battery` — overall battery state plus per-module BMS detail (SOC, voltage,
    temperature, capacity, cycle count).
  - `/mode` — current battery mode, reserve/target SOC, charge/discharge rates,
    and active automation flags (Cosy/Agile/auto-winter/load limiter).
  - `/version` — app version, device type, inverter serial, and ARM/DSP firmware.
  - `/help` — lists all available commands.
- **Command menu autocompletion** — the bot registers its commands with Telegram
  on startup, so typing `/` in the chat suggests the available commands.

### Security

- **Telegram bot now allowlists the configured chat** — previously `/status`
  replied to *any* chat that messaged the bot. Commands now only execute for
  the chat id set in `telegram_chat_id`; all other chats are silently ignored.
  This is a prerequisite for adding inverter control commands safely.

## [0.30.0] - 2026-06-18

### Added

- **ntfy push notifications** — a new notification channel alongside Telegram
  and WhatsApp. Free via ntfy.sh, install the app on your phone and subscribe
  to your auto-generated topic. The topic name is based on your inverter serial
  so it's unique to you. Shown with a copy button for easy setup.
- **Alert logs now visible at default log level** — successes, failures, and
  debounce suppression all log at `warn` level so you can see what's happening
  without changing `RUST_LOG`.

### Fixed

- **Grid Offline alert now matches the Status page** — the Status page banner
  fires on `grid_loss || !grid_online`, but the alert only checked `grid_loss`.
  If the AC voltage register showed no grid but the consensus flag was off, the
  alert stayed silent. Now both conditions trigger a notification.
- **Toggling alerts now takes effect immediately** — saving alert settings
  clears the debounce so re-enabled alerts fire on the next poll cycle without
  waiting for the cooldown to expire.

### Changed

- **Cooldown moved to Alert Triggers section** — renamed from "Other Alerts"
  to "Alert Triggers & Cooldown" to make clear the cooldown applies to all
  channels, not just Telegram.
- **ntfy listed before WhatsApp in Settings** — with a recommendation badge
  since WhatsApp pairing can be unreliable for self-send delivery.
- **Removed Solar Clipping and PV String Loss alerts** — both were unreliable
  and produced false positives.

## [0.29.0] - 2026-06-18

### Fixed

- **Grid Offline alert now matches the Status page** — the Status page banner
  fires when the grid drops (either `grid_loss` or `!grid_online`), but the
  alert only checked `grid_loss`. If the consensus flag was off but the AC
  voltage register showed no grid, the alert stayed silent while the banner
  screamed. Now both conditions trigger a notification.
- **Toggling alerts now takes effect immediately** — saving alert settings
  clears the debounce so previously-fired alerts re-trigger on the next poll
  cycle. No more toggling off/on and waiting 30 minutes.
- **Alert logs now visible at default log level** — send confirmations,
  debounce suppression, and evaluation diagnostics all log at `warn` level
  instead of `info`, so they show up without changing `RUST_LOG`.

### Changed

- **Cooldown moved to Alert Triggers section** — renamed from "Other Alerts"
  to "Alert Triggers & Cooldown" so it's clear the cooldown applies to all
  channels, not just Telegram.
- **Removed Solar Clipping and PV String Loss alerts** — both were unreliable
  and produced false positives.

## [0.28.11] - 2026-06-18

### Fixed

- **PV String Loss false alerts** — the alert only checked PV power, so a
  string with voltage but low power (shading, breaker off) would falsely
  trigger "string lost". Now also checks voltage: if PV voltage is above
  50V the string is clearly connected and won't alert.
- **Developer mode toggle in E2E tests** — the toggle is a `<div>` element,
  not a `<button>`. Three inline test selectors were silently clicking nothing,
  causing 8 timeout failures. Fixed all three to target `div.cursor-pointer`.
- **Strict-mode selector violations** — 12 tests failed because Playwright
  found multiple matching elements (e.g. `text=1h` matching both an `<option>`
  and a `<button>`). Added `.first()` or more specific selectors throughout.
- **Outdated battery page references** — "Stored Energy", "Capacity" and
  "Available" test expectations removed — these labels were removed in a
  prior UI refactor. Replaced with checks for the SOC ring and Charged Today.
- **Filter placeholder ellipsis mismatch** — the Logs page input uses
  `Filter logs…` (Unicode ellipsis) but the test looked for three dots.
- **Removed Solar Clipping and PV String Loss alerts** — both were unreliable
  and produced false positives. Grid Offline, battery temperature, and SOC
  alerts remain.

### Changed

- **E2E test count now accurate** — many failures were false positives from
  stale selectors. All 22 failures fixed, 217 tests should now pass.

## [0.28.10] - 2026-06-18

### Fixed

- **Android build fails** — `WebviewWindow::set_icon()` is desktop-only in Tauri 2.
  Gated behind `#[cfg(desktop)]` so the Android APK build no longer fails.

## [0.28.9] - 2026-06-17

### Fixed

- **CI: tauri-cli install fails on latest nightly** — the `rustix` crate (a
  transitive dependency of tauri-cli) uses internal `rustc_layout_scalar_valid_*`
  attributes that newer nightly Rust rejects. Set `RUSTC_BOOTSTRAP=1` during
  `cargo install tauri-cli` to bypass the feature gate check.

### Changed

- **CI: Android APK now builds in parallel** — removed `needs: [build]`
  from the `build-android` job so it starts alongside desktop targets instead
  of waiting for them all to complete first.

## [0.28.8] - 2026-06-17

### Added

- **Persistent WhatsApp sessions** — pairing and Signal sessions now survive
  restarts. Uses a custom rusqlite-backed store (`whatsapp-store.db`) instead
  of the previous in-memory-only backend. No more re-pairing after every
  restart. The database is stored alongside settings.
- **Send retry with session warm-up** — the first message attempt after
  connecting can trigger the Signal pre-key exchange as a side effect.
  If it fails, the app retries up to 3 times with 3-second delays. This
  fixes the "session not found" errors where the initial sync hadn't
  completed.
- **Android APK build pipeline** — new `build-android` job in the release
  workflow cross-compiles with the Android NDK for `aarch64` and `x86_64`,
  producing APKs that run on Chromebooks via ChromeOS's Android subsystem.
  Requires Java 17, Android SDK, and the NDK (installed in CI).

### Fixed

- **Stale session database cleaned on logout** — when you unlink the device
  from WhatsApp, the stored session database is deleted so a fresh pairing
  starts cleanly next time.
- **Battery SOC chart showing 0–5000 scale instead of 0–100** — the
  shared Y-axis lock feature was applied to *every* history chart,
  overriding SOC's fixed `[0, 100]` domain with a shared ceiling computed
  from the largest value across all charts. The lock is now scoped to the
  Solar PV chart only; SOC stays fixed at `[0, 100]`.
- **Docker build failing on stable toolchain** — added `rustup toolchain
  install nightly && rustup default nightly` to the Docker build stage.
- **CI failing on stable toolchain** — switched `dtolnay/rust-toolchain`
  from `@stable` to `@nightly` in CI workflow.
- **regdump example broken after crate rename** — updated all `app_lib::`
  references to `givenergy_local::` in `examples/regdump.rs`.
- **Mobile layout broken in Settings alerts section** — simplified the
  Battery Temperature & SOC panel to a flat 2-column grid and fixed button
  sizing on narrow viewports.

### Changed

- **WhatsApp pairing now persists across restarts** — the amber warning
  about re-pairing has been removed from the Settings page. Pair once, it
  sticks until you manually unlink.

## [0.28.7] - 2026-06-17

### Added

- **Telegram Bot /status command** — send `/status` in your Telegram chat
  and the app replies with a live snapshot of your system (battery charge,
  solar generation, grid power, home usage).
- **WhatsApp native client (experimental)** — the app can now pair directly
  with your WhatsApp account (like WhatsApp Web). QR code in Settings, alerts
  delivered directly through WhatsApp. Note: you'll need to re-pair after
  every restart for now.
- **Choose where WhatsApp alerts go** — you now enter the phone number that
  should receive alerts (a different number from the linked account). Previously
  the app tried to send to itself, which silently failed.
- **"All clear" notifications** — when a triggered alert (e.g. high battery
  temperature) returns to normal, you'll get a resolution notification.

### Fixed

- **Battery SOC chart scale** — the battery charge chart was sometimes showing
  a 0–5000 scale instead of the correct 0–100%. This happened because a recent
  feature that locks the Y-axis on the solar chart was accidentally applying to
  all charts. It's now restricted to the solar chart only.
- **WhatsApp messages going nowhere** — the app was trying to send alert
  messages to its own WhatsApp account, which doesn't work (the encryption
  handshake hasn't completed for the freshly-linked device). Now messages go
  to the phone number you specify in Settings.
- **Reduced noise in the logs** — harmless WhatsApp encryption warnings
  ("Failed to encrypt for device") are now suppressed from the console output.
  They're expected behaviour with an in-memory session store.

## [0.28.6] - 2026-06-17

### Fixed

- **Cost calculation overcount from cumulative counter spikes** — when a
  dongle corruption spike hit `today_import_kwh`, the delta was zeroed but the
  `prev` value was updated to the spike. This permanently inflated the
  cumulative baseline, causing every subsequent bucket to compute wrong deltas.
  Now when delta > 2 kWh, `prev` is not updated at all. The next real reading
  produces a catch-up delta (capped at 2), then `prev` re-syncs. Spike damage
  is limited to at most one bucket instead of persisting forever.

## [0.28.5] - 2026-06-17

### Added

- **Lock Y-axis scale setting** — a new toggle in Settings → Panel Controls
  keeps chart vertical scales stable across time range switches. When enabled,
  the Y-axis ceiling is computed from the data maximum and the highest ceiling
  seen during the session is shared across all ranges (so switching from 1h to
  30d never shrinks the scale). Applied to the Solar tab's PV chart and the
  History tab's Solar PV chart. The SOC chart was already at a fixed [0, 100].
  ([#81](https://github.com/psylsph/home-energy-manager/issues/81))

### Fixed

- **Dongle memory-leak fingerprint detection missed 0xFFC0/0xFFE0 corruption
  patterns** — the `is_block_suspicious()` check only matched against 17 known
  fingerprint values at specific register positions. A different dongle memory
  region was producing corruption at 0xFFC0–0xFFE0 across multiple registers,
  which the fixed-position fingerprint missed entirely. Added a general
  heuristic: if 10+ registers in a 60-register block have values ≥ 0xE000
  (57344), the block is almost certainly leaked memory. This catches the
  0xFFC0/0xFFE0 pattern without needing to enumerate every possible corrupt
  value. ([#76](https://github.com/psylsph/home-energy-manager/issues/76))

- **Lifetime energy totals could decode as enormous values from uint32
  register misassembly during dongle corruption** — added a hard plausibility
  floor (`decode_lifetime_total_kwh()`) that returns 0.0 when the hi register
  exceeds 1000 (corresponding to > 6.5 GWh lifetime, impossible for
  residential). Applied to all 12 lifetime total decode sites across single-
  phase, three-phase, and Gateway decoder paths. The sanitizer catches these
  anyway, but the decoder-level check prevents the f32 value from entering the
  pipeline in the first place.

- **AC charge/discharge limit carry-forward logged at WARN instead of INFO**
  — when the AC config block (HR 300-359) is briefly unavailable, the system
  correctly preserves the previous limit value. The logging was at WARN level,
  filling the developer console with noise on every transient skip. Changed to
  INFO so it only shows at the INFO capture level.

## [0.28.4] - 2026-06-17

### Fixed

- **Full-day discharge slots (00:00–23:59) incorrectly treated as "suspiciously
  small" and replaced with previous slot values** — the slot sanitizer was
  checking only whether the start time was ≤ 00:10, not the actual duration.
  A valid force-discharge or timed-export window spanning the full day was
  overwritten with the previous slot, making force discharge look like it
  briefly applied then reverted. Now checks duration (must be ≤ 10 minutes)
  in addition to start time. ([#82](https://github.com/psylsph/home-energy-manager/issues/82))

- **Pause Battery button showed success but inverter kept exporting** — the
  "Eco Paused" action was clearing charge/discharge flags and restoring eco
  mode, but never raising the SOC reserve to 100%. The inverter returned to
  eco mode with a 4% reserve and continued discharging. Now writes reserve=100
  so the battery actually stops exporting. ([#82](https://github.com/psylsph/home-energy-manager/issues/82))

### Changed

- **Force Charge now writes an active charge slot when minutes are provided**
  — matching GivTCP's working implementation, the backend accepts an optional
  `{ minutes: N }` body and writes a charge slot covering now → now+N minutes
  before setting the enable charge flags. Without a slot, some hardware would
  show the button state as active but never actually begin charging.
  ([#82](https://github.com/psylsph/home-energy-manager/issues/82))

- **Discharge slot hint now clarifies client-local storage** — the yellow
  callout in Eco mode now says "Slots are saved only to this device/client
  until you switch" instead of "saved to the inverter", since edits are held
  in browser localStorage until Timed mode is activated.
  ([#82](https://github.com/psylsph/home-energy-manager/issues/82))

## [0.28.3] - 2026-06-16

### Fixed

- **Charge slot target SOC silently dropped on AC-coupled/Gen1/Gen2 inverters**
  — the per-slot `target_soc` slider in the Charge Schedule editor was only
  persisted to the inverter on models with extended schedule slots (Gen3+
  hybrid, three-phase, AIO, HV Gen3). On AC-coupled, Gen1, and Gen2 models
  the value was accepted by the UI and the response was "Saved", but neither
  `enable_charge_target` (HR20) nor `charge_target_soc` (HR116) were ever
  written — the battery would always charge to 100% regardless of the slider
  position.
  
  The backend now writes the target SOC to the standard HR116 register and
  sets `enable_charge_target=1` when saving a charge slot with an explicit
  target below 100% on these models. For `target_soc=100` ("charge to full")
  the existing behaviour is preserved (flag cleared, no write).
  ([#82](https://github.com/psylsph/home-energy-manager/issues/82))

- **Discharge slot target SOC slider shown on unsupported models** — the
  target SOC control in the Discharge Schedule editor was displayed on all
  models, but only takes effect on inverters with extended schedule slots
  (Gen3+ hybrid, three-phase, AIO, HV Gen3, Gen4). On AC-coupled, Gen1, and
  Gen2 inverters there is no register to write a per-slot or global discharge
  target SOC, so the slider was silently inoperative. It is now hidden on
  models where `max_discharge_slots <= 2`.

## [0.28.2] - 2026-06-16

### Added

- **Unit-test infrastructure for React hooks** — `vitest` + `@testing-library/react`
  `jsdom` for component-level unit tests. Run with `npm test` (`vitest run`)
  or `npm run test:watch`. Separate `vitest.config.ts` keeps the production Vite
  config untouched.
  - First test suite: `src/hooks/useAction.test.tsx` with 9 tests covering
    loading/success/error timing, render stability, cycle repeats, and the
    timeout cleanup behaviours below.

### Fixed

- **Uncontrolled timeout in `useAction()` hook** — the feedback-clearing
  `setTimeout` was not tracked, causing:
  - Stacking on rapid button clicks (multiple timeouts racing to update state).
  - `setState` on an unmounted component if the component unmounted while a
    timeout was pending.
  
  The hook now uses a `useRef`-tracked timer that is cleared on every new
  request and on unmount (`useEffect` cleanup, which only calls `clearTimeout`
  — never `setState` — so it does not trip the `react-hooks/set-state-in-effect`
  lint rule). Extracted to `src/hooks/useAction.ts` for testability.

- **Gen3 Hybrid (0x20xx) false positive grid-loss detection** — the standard
  detection path used OR logic (`system_mode == OffGrid || no_utility_bit`),
  causing transient register fluctuations to trigger false `grid_loss = true`
  even when grid AC voltage/frequency readings showed the grid was present.
  
  All non-AC device types now use the actual grid voltage/frequency readings
  as a corroborating AND check: both the software register(s) AND the electrical
  readings must agree before grid loss is reported. This aligns the single-phase
  path with how three-phase and Gateway models already work, and mirrors the
  AC-coupled voltage/frequency approach.

## [0.28.1] - 2026-06-16

### Fixed

- **AC-coupled inverters show false "Grid power lost" alert** — v0.26.3
  switched grid-loss detection to use the `system_mode` register (IR 49), which
  is correct for hybrid inverters but causes AC-coupled models to falsely
  report grid offline during normal operation. AC-coupled now uses the actual
  AC voltage and frequency readings to determine grid presence, matching the
  same approach used by three-phase models.
  ([#83](https://github.com/psylsph/home-energy-manager/issues/83))

## [0.28.0] - 2026-06-16

### Added

- **Panel Graphs controls in Settings** — the Settings page's "Panel
  Visibility" section is now a broader "Panel Controls" section with two
  sub-sections. The existing nav-panel visibility checkboxes move under a
  "Panel Visibility" heading, and a new "Panel Graphs" sub-section adds:
  - A **Show Graphs** toggle that hides/shows the trend charts on the
    Battery and Solar tabs.
  - A **Time Scale** selector (Today / Rolling 24H) that switches those
    same charts between a calendar-day view and a rolling last-24-hours
    view.
  Both preferences are remembered per-device (localStorage) and apply
  instantly — no restart or save needed.
  ([#81](https://github.com/psylsph/home-energy-manager/issues/81))

### Changed

- **Battery/Solar tab charts honour the new Panel Graphs settings** —
  `BatterySocChart` and `SolarPowerChart` now read their time scale from the
  store (Today vs Rolling 24H) instead of being hardcoded to today, and
  query the history backend with the appropriate rolling flag. Their titles
  update to match (e.g. "SOC Today" / "SOC — Last 24h"). The Power and
  History graphs are untouched.

## [0.27.0] - 2026-06-16

### Added

- **Solar Power Today chart on the Solar tab** — replicates the History →
  Solar "PV Power (W)" chart, pinned to today, so the Solar tab now carries
  its own solar-output trend. PV2 auto-detects from history (no second
  string shown for single-string owners). The Power and History solar
  graphs are untouched. ([#81](https://github.com/psylsph/home-energy-manager/issues/81))
- **SOC Today chart on the Battery tab** — replicates the History → Battery
  "SOC %" chart, pinned to today, giving the Battery tab a SOC-over-time
  trend that the Status page doesn't have. ([#81](https://github.com/psylsph/home-energy-manager/issues/81))
- **Shared post-query spike filter** — `removeSpikes` and
  `SPIKE_THRESHOLDS` (previously module-local in `HistoryPage.tsx`) moved to
  `src/lib/chartSeries.ts` so every chart that renders raw polled series
  applies identical spike filtering.

### Changed

- **Bottom navigation order** — Solar now appears before Inverter, matching
  the left-to-right status-gathering flow. ([#81](https://github.com/psylsph/home-energy-manager/issues/81))
- **Default window width** reduced from 980 to 850 px.

### Fixed

- **Register-corruption saturation values are never released** — the
  register sanitizer now treats int16-saturation power readings (|v| ≥
  32 000, the documented dongle memory-leak fingerprint of ±32767) as never
  legitimate: they're always replaced with the previous reading (or
  sign-preserved clamped to the limit if there is none) and never accepted
  after the 10-cycle suspect window. Previously a stuck `32767` would be
  permanently accepted after ~10 min and poison the history database and
  UI. Complements the existing block-level fingerprint check.
- **Control commands route from a single consistent device-type view** —
  every control handler (charge/discharge rate, reserve, force
  charge/discharge, pause, slot set) now derives its AC-coupled vs
  three-phase routing flags from one locked snapshot view via new
  `latest_device_type` / `device_type_flags` helpers, instead of locking
  the snapshot twice independently. The previous double-lock could race
  with the poll loop (the snapshot updates between the two locks) and pick
  the wrong register set, writing to single-phase registers on a
  three-phase unit or vice versa.

## [0.26.3] - 2026-06-16

### Fixed

- **Inverter fault detection uses authoritative registers** — grid loss,
  inverter trip, and battery over-temperature are now decoded from the
  inverter's self-declared status registers instead of the unverified
  IR(40) fault-word bits (whose bit layout givenergy-modbus documents
  as "not verified against official firmware docs"). The previous
  implementation keyed on the wrong bits (bit 8 = "Inverter Current
  Fault" rather than bit 7 = "No Utility", etc.).
  - `grid_loss` now uses **IR(49) `system_mode` == OFF_GRID** (the
    `WorkMode` enum) as the primary signal, corroborated by the IR(40)
    bit 7 "No Utility" fault bit.
  - `inverter_trip` now uses **IR(0) `status` == FAULT** (the `Status`
    enum).
  - `battery_over_temp` now uses **IR(57) `charger_warning_code` == 1**.
- **All register writes routed through the encoder whitelist** — the
  Eco-mode and PauseBattery discharge-slot clearing, and the charge-slot
  force-charge-flag clearing, now go through the encoder's
  `SAFE_WRITE_REGS`-validated commands instead of constructing raw
  `RegisterWrite` structs that bypassed the security invariant. A new
  `ClearChargeTargetFlag` command and `clear_discharge_slot_writes()`
  helper centralise this.
- **Configuring a charge schedule slot no longer leaves force-charge
  asserted** — clearing the stale charge-target flag (HR 20) when a
  schedule charge slot is enabled prevents `snapshotForceCharge`
  (`enable_charge && enable_charge_target`) from staying asserted.
- **Three-phase PauseBattery now fully resets export state** — clears
  force charge/discharge + AC charge flags and restores Eco power mode
  in a single validated `ThreePhaseCosyExit` batch (previously left the
  `HR_BATTERY_POWER_MODE` write and three-phase flag clearing as
  separate raw writes).
- **Grace-period baseline survives all-NaN readings** —
  `GraceCumulativeSamples` cumulative-counter fields are now `Option<f32>`;
  fields whose median can't be computed (every grace sample was `NaN`)
  are left untouched instead of poisoning the delta-check baseline with
  `NaN`.

### Changed

- **Deduplicated app startup** — extracted shared `init_tracing()` and
  `initialize_app_state()` helpers so the Tauri-windowed `run()` and
  headless `run_headless()` paths can no longer diverge (they had
  already started to, e.g. `blocking_lock()` vs `.lock().await`).
- **Solar page PV layout** — the PV1 card now spans the full width when
  no PV2 string is connected, instead of rendering an empty "No PV2
  input detected" placeholder card.

### Removed

- Obsolete ADR `why-gateway-is-not-the-right-approach-for-parallel-aios.md`.

## [0.26.2] - 2026-06-16

### Fixed

- **Gateway display cleanup** — battery voltage, current, temperature,
  inverter temperature, PV voltage, and module count now show `—`
  instead of `0.0` / `0` on the Status, Battery, Inverter, and Solar
  pages when the data isn't available from the Gateway.
- **formatVoltage / formatCurrent** now return `—` for NaN values
  (matching the existing formatTemp behaviour).

## [0.26.1] - 2026-06-16

### Changed

- **Battery node colour reflects SOC** on the energy flow diagram — the
  battery lozenge border and its SOC/mode text now change colour based on
  battery level: green (≥50%), amber (20-49%), red (<20%).
- **Status page layout** — Today summary panel moved to the left, Battery
  panel to the right (swapped positions).
- **Energy flow node unit text** — third row (unit/device type) made bold
  and slightly larger; dropped down 4px for better spacing. Unit text
  colour now matches the node accent colour instead of secondary grey.

## [0.26.0] - 2026-06-15

### Added

- **GivEnergy Gateway support** *(experimental)* — first-class support for the
  GivEnergy Gateway (DTC 0x7001, serial prefix `GW`), a system controller
  / AC distribution hub that manages up to 3 All-in-One battery units.
  - **Polling & decoding**: reads the Gateway's unique Input Register bank
    (IR 1600-1859) for grid voltage, PV generation, house load (excl. EV
    charger), aggregate battery SOC/power/energy, and per-AIO detail.
  - **Power-flow diagram**: integrates into the existing live flow diagram
    with correct sign conventions, including grid power derived from energy
    balance (the Gateway has no direct grid-power register).
  - **Identity & diagnostics**: device detail card showing software version
    (GA000009+), V1/V2 firmware variant, work mode, configured vs online
    AIOs, per-AIO SOC/power/serial/energy, and decoded fault codes.
  - **Control path**: full schedule, mode, and rate-limit control via the
    three-phase register set (HR 1110/1108/1109/1111, HR 1113-1121,
    HR 1122/1123) — matching the GivTCP reference implementation.
  - **10-slot scheduling**: support for extended charge/discharge schedules
    with per-slot target SOCs (slots 3-10 via HR 240-299).
  - **History**: daily and lifetime energy totals chart correctly via the
    Gateway's authoritative registers.
  - **Smart features**: Agile Octopus and Cosy tariff integrations both
    route through the correct three-phase force-charge/discharge registers.
  - **UI indicators**: "house load excludes EV" hint on the consumption tile;
    battery temperature and per-cell data noted as unavailable (requires
    direct AIO connection, not yet supported).

### Changed

- **`supports_schedule_slots()`** — Gateway now returns `true` (was in the
  batteryless exclusion set), enabling schedule-slot configuration.
- **`uses_three_phase_schedule_slots()`** — now includes Gateway, routing
  all slot, force-charge, and rate-limit commands through the three-phase
  register map.

### Fixed

- **`formatFrequency`** — now returns `—` (em dash) for NaN / Infinity
  values instead of displaying "NaN Hz".

## [0.25.3] - 2026-06-15

### Fixed

- **Window opens at the top of the screen** — the app no longer centres
  itself on an OS-calculated screen position. Instead it opens at the
  top-left of the primary monitor, preventing the bottom edge from being
  hidden behind the taskbar on 1080p displays. Also requests focus so the
  window appears in front of other windows when launched. (#79)

## [0.25.2] - 2026-06-15

### Fixed

- **Stale frontend after upgrade** — `index.html` and other non-hashed
  static files now include `Cache-Control: no-cache` headers, so the
  embedded WebView always revalidates before serving cached content.
  Previously the absence of any caching directive allowed the WebView's
  heuristic cache to reuse a stale `index.html` across app restarts after
  an upgrade, which pointed at the old version's hashed asset names and
  showed the previous UI until the user force-refreshed (Ctrl+F5). Vite
  content-hashed assets under `/assets/` are marked `immutable` with a
  year-long expiry since their filenames change whenever the content does.
  (#80)
- **Date picker closes when a day is selected** — on both the History and
  Power pages, the native date/month picker now auto-dismisses as soon as
  you pick a day, but stays open while browsing months or years (the old
  blanket blur closed it during navigation as well).

## [0.25.1] - 2026-06-15

### Changed

- **Inverter clock moved to the Inverter tab** — the inverter's wall-clock
  time has moved out of the top bar and into the bottom of the Device Info
  panel on the Inverter tab, where it sits next to the other device details.
- **Status page battery panel now matches the Battery tab** — the Status
  page battery card is now the same detailed panel used at the top of the
  Battery page (power, voltage, current, temperature, mode, reserve, and
  today's charge/discharge energy).
- **Battery and Today cards are now equal height** on the Status page when
  shown side by side.
- **Top bar content is now vertically centred.**

### Fixed

- **"Discharged Today" no longer wraps** in the battery panel — the label
  column now sizes to fit.
- **Tighter energy flow diagram** — cropped the empty space above and below
  the power-flow symbols on the Status page.

## [0.25.0] - 2026-06-15

### Added

- **Inverter clock shown in the top bar** — The inverter's own wall-clock
  time now appears under the connection status, exactly as the inverter
  reports it (no timezone conversion). Handy for spotting clock drift or a
  DST mismatch that could throw off your charge schedules.
- **Step through periods on the Power tab** — The Power page now has the
  same Older / Newer buttons as the History page, so you can look back at
  previous hours, days, weeks, or months.
- **Date picker for the current period** — On both Power and History, the
  period label is now a date picker for the Today and Month ranges. Pick a
  day or month to jump straight there instead of tapping Older over and
  over.

### Fixed

- **"Today" charts no longer start at 1am on remote servers** — If you run
  the app headless or in Docker and view it from a different timezone, the
  Today view was querying the server's idea of "midnight" while showing your
  browser's midnight — so every Today chart started an hour out. The browser
  now tells the server exactly which window to query, so the data always
  lines up with the axis you see, regardless of server timezone.

### Changed

- **Battery page now shares the Status page's battery panel** — same
  component, consistent layout.
- **Cold-battery warning added to the Status page.**
- **Slightly shorter default window height.**

## [0.24.x] - 2026-06-01

### Changed

- **Energy flow diagram redesigned for mobile** — new X-shaped layout with
  lozenge-shaped nodes, bigger symbols and fonts on phones, and clearer
  flow direction (a `-` prefix shows when the grid is exporting or the
  battery is discharging). Battery mode and SOC merged into one row.
- **Energy flow symbols now respect light mode** (previously stayed dark).
- **Power report relabelled "Consumption Report"** (rich layout kept).
- **Clearer CSV/PDF export confirmations** ("downloaded to Downloads").
- **Windows installer security notice** documenting the unsigned-MSI
  SmartScreen warning and a recent Defender false positive.

### Fixed

- **Fewer false data-corruption warnings** — daily consumption is derived
  from several registers that can wobble by a tick or two; tiny wobbles are
  now treated as noise instead of corruption. Repetitive corruption warnings
  are also downgraded after the first few, and a near-zero baseline clamp bug
  was fixed.
- **Grid power no longer rejected during EV charging** — ceiling raised from
  ±10 kW to ±15 kW (a 100A fuse allows well over 10 kW).
- **SOC=100 only rejected when actually charging hard** (threshold raised
  from 500 W to 2000 W — gentle top-balancing is normal).

## [0.23.x] - 2026-06-01

### Added

- **Grid loss, inverter fault & battery over-temperature detection** —
  surfaced as a persistent red banner across the app, an inline warning on
  the Status page, and optional browser notifications (Settings toggle) with
  restoration messages.

### Changed

- **Windows releases dropped the experimental MSIX path** and ship the
  supported MSI installer only.

### Fixed

- **Force Discharge / Stop Charge now actually stop** — they restore Eco
  mode instead of pausing, which had left the inverter exporting to the
  grid. Fixes
  [#72](https://github.com/psylsph/home-energy-manager/issues/72).

## [0.22.x] - 2026-06-01

- Briefly added a Windows MSIX release asset, then restored the MSI-only
  Windows build after the MSIX bundle target broke release CI.

## [0.21.x] - 2026-06-01

### Added

- **Read-only GivEnergy EV Charger monitoring** — local Modbus polling of
  the GE EV charger on port 502, with live updates and a Home → EV branch on
  the energy-flow diagram. Optional charger address + network scan in
  Settings.
- **Developer-only BMS diagnostics** on the Battery page (raw per-module
  status/warning registers).

### Changed

- Advanced port fields hidden unless Developer Mode is on.
- ROADMAP expanded with multi-zone tariff and scheduling plans.

### Fixed

- **EV charger polling compatibility** — correct unit id and
  GivTCP-compatible state mapping.

## [0.20.x] - 2026-06-01

- **Power report relabelled "Consumption Report"** (KPI cards, breakdown
  charts, highlights, and bucket table all kept).
- **Clearer CSV/PDF export confirmations** ("downloaded to Downloads").

## [0.19.x] - 2026-06-01

### Added

- **Power page exports** — CSV and printable PDF/HTML reports for the
  selected range, with KPI cards, breakdown charts, highlights, and
  estimated kWh totals.
- **Smarter external meter detection** — keeps retrying for up to 17 minutes
  for LoRA-linked meters that are slow to respond at startup.
- **Mobile time-range picker** — compact dropdown on phones, pill buttons on
  wider screens.

### Changed

- **Faster three-phase polling** — drops ~300ms of redundant register reads
  per cycle.
- **Gentler request pacing** on three-phase dongles (250ms gap between
  requests).
- **Quieter logging** — single dropped frames at debug, not warning.
- **Relaxed meter detection** accepts any non-zero voltage (was ≥100V).

## [0.18.x] - 2026-06-01

### Changed

- **Battery Calibration now checks the BMS firmware, not the inverter model**
  — what matters is the battery, not the inverter. Gen1/Gen2 batteries (BMS
  firmware below 3000) get the button; Gen3+ auto-calibrate via the BMS.
  Falls back to the old model check when BMS firmware isn't readable.

### Fixed

- **All-in-One control readbacks** — AC charge/discharge limits, export
  priority, EPS, and pause slot now show correctly instead of blanking out.

## [0.17.x] - 2026-06-01

A large series of releases focused on **HV/three-phase battery support,
chart reliability, and UI polish.**

### Added

- **HV stackable battery support** (GIV-BAT-*-HV): three-phase and HV
  inverters now show real battery readings via the BCU/BMU protocol, with
  per-cell voltage and temperature detail. The app also echoes dongle
  heartbeats so the socket no longer drops every ~9 minutes.
- **Panel visibility toggles** in Settings — hide Power, Battery, Solar,
  Meters, or History from the bottom nav.
- **Load Discharge Limiter** moved out of Developer Controls to the Control
  page (visible in Eco mode).
- **Clickable chart legends** (mute a series) and a **shared time range**
  between Power and History pages.
- **CT clamp configuration card** on the Meters page.
- **iOS / PWA polish**: Apple touch icons, standalone mode, safe-area
  insets, and a README guide for remote access via Tailscale.
- **`GET /api/logs?after=`** for incremental log polling.

### Changed

- **Battery temperature always comes from the BMS**, not the inverter's
  unreliable temperature register.
- **Cosy charging now writes into the inverter's own schedule** (survives an
  app crash) and preloads the next slot ahead of time.
- **SOC values below 4% corrected on read.**
- **Discharge schedule always visible**, even in Eco mode — edits are held
  locally until you switch to Timed mode.
- **Far fewer log messages by default** (WARN instead of INFO).
- **Refresh interval is now a button group** (5/10/15/20s).
- **Proper HTTP status codes** (400 vs 500) and settings saved to disk
  before the in-memory copy updates.

### Fixed

- **24h history chart now starts at local midnight** (was 01:00 in BST);
  short ranges still trim to the first data point.
- **Three-phase battery voltage** no longer stuck at 0V / 0°C, and no longer
  triggering false winter mode from missing data.
- **Gen3 charge slot 2** read from the correct extended register (was reading
  stale data). Fixes
  [#51](https://github.com/psylsph/home-energy-manager/issues/51).
- **Gen3 stuck in Timed mode** — switching to Eco now clears discharge slot
  registers. Charge slots no longer blank out after mode switches.
- **Cumulative-counter baselines self-correct** after 10+ consistent
  corrections instead of locking onto a bad value forever.
- **F5 refresh no longer 404s** — switched to a hash router. Fixes
  [#59](https://github.com/psylsph/home-energy-manager/issues/59).
- **`/api/logs` crash** on a partially-filled ring buffer, plus many
  read-path and sanitiser robustness fixes (frame resync, exception-code
  reading, HHMM validation, etc.).
- **Lots of performance work**: 60-register reads (was 20), settings loaded
  once per cycle, history queries moved off the worker thread.

## [0.16.x] - 2026-06-01

### Added

- **Status page shows the active Agile slot** ("Agile: charging" /
  "discharging").

### Fixed

- **Dongle comms completely rewritten** — a background listener now routes
  responses by content, so stray or wrong-device frames no longer cascade
  into mismatches and timeouts.
- **Cosy writes were silently failing** (a write-response matching bug) —
  fixed, along with crash recovery, mode-switch cleanup, and stop-charge
  button accuracy.
- **Settings file no longer corrupted** by concurrent writes (atomic save).
- **Chart lines now reach the axis** (x-axis starts at the first reading).

## [0.15.x] - 2026-06-01

### Added

- **Lifetime import/export totals** (Inverter page) with sanitisation.
- **Per-slot discharge target SOC** on extended-schedule models.
- **Agile price caching + rolling 24-hour window** so the forecast doesn't
  blank out when Octopus starts publishing tomorrow's data; auto-refreshes
  every 5 minutes.

### Changed

- Meters labels clarified (Import/Export Total).
- Note-box and callout text readable in light mode; active Agile slot
  highlighted with a pulsing red border.

## [0.14.x] - 2026-06-01

### Added

- **Charging Mode selector** — Standard / Cosy (beta) / Agile (beta).
- **Agile Octopus integration (beta)**: postcode → region detection, price
  thresholds, a colour-coded 12×4 forecast grid, and a backend state machine
  that auto force-charges/discharges based on price.
- **Cosy charging on three-phase inverters.**

### Changed

- **Force Charge / Discharge are toggle buttons** reflecting live state.
- **Pause Battery matches GivTCP** (clears charge + discharge flags; no
  longer sets SOC to 100%); status shows "Override" when forcing.
- Force charge/discharge now write the correct three-phase registers.

## [0.13.x] - 2026-06-01

### Added

- **Calendar month view** for history, plus Older/Newer paging.
- **Three-phase / HV dashboard support** — real solar, grid, battery and
  daily-energy values instead of zeros (model locked after first detection
  so a bad read can't flip it).
- **Three-phase schedule editing** (native registers + extended slots).
- **Smarter external-meter detection** (skipped on three-phase).

### Changed

- **Evenly-spaced chart labels** across all ranges; consistent app name
  ("Home Energy Manager").
- **Mobile-friendly Quick Actions** and larger tab-bar icons on phones.

### Fixed

- **Write framing bug** — every write was 36 bytes (a double CRC) and was
  silently ignored by the dongle, so commands didn't go through. Now correct
  at 34 bytes.
- Three-phase home power/consumption and daily solar totals (now read as a
  combined 32-bit value instead of summing two corruptible 16-bit
  registers). Fixes
  [#43](https://github.com/psylsph/home-energy-manager/issues/43).
- Three-phase firmware display. Fixes
  [#48](https://github.com/psylsph/home-energy-manager/issues/48).
- **Changing the refresh rate no longer drops the connection.**
- **Debian dock icon** now matches the app name.
- **Tests no longer touch your real settings file.**
- Cosy exit no longer traps you in Timed mode; Cosy survives a crash; tiny
  daily-energy dips no longer false-alarm.
- macOS DMG `/Applications` workflow with automated Gatekeeper handling.

## [0.12.x] - 2026-06-01

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

## [0.11.x] - 2026-06-01

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

## [0.10.0] - 2026-06-01

### Changed

- **Renamed to Home Energy Manager**: The app is now presented as "Home
  Energy Manager" instead of "GivEnergy-Local". Installer names, start menu
  entries, and browser tab titles all updated. The executable remains
  `givenergy-local` and existing settings/history are preserved.

### Fixed

- **Reserve SOC lower bound**: Consistently enforced at 4% (inverter-safe
  minimum), not 0%.

## [0.9.x] - 2026-06-01

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

## [0.8.x] - 2026-05-01

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

## [0.7.0] - 2026-05-01

- **Connected clients display**: Settings page shows WebSocket client IPs.
  Local connections labelled "This device".
- **FAQ.md**: Firewall, LAN access, macOS downloads, network scanning.
- **LAN access in dev mode**: Axum server serves frontend from `dist/`.
- **Network Access shows LAN IP**: Real LAN IP instead of 127.0.0.1.

## [0.6.0] - 2026-05-01

- **Developer Mode toggle**: Reveals a Logs page with scrollable,
  filterable backend output. Log capture, text/level filtering, auto-scroll.
- **Network discovery filtering**: Verifies GivEnergy protocol before
  listing a device (no more false positives from other port 8899 services).

## [0.5.x] - 2026-05-01

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

## [0.5.0] - 2026-05-01

### Added

- **History page**: 5 metric tabs (Battery, Solar, Grid, Home, Cost) with
  time-series charts. SQLite-backed storage. 7 time ranges (1h to 1y).
  Older/Newer navigation. Configurable import/export electricity tariffs.
- **Headless server mode**: Run without a window on Linux
  (`--headless`, `--port`, `--dist`).
- **98 Rust unit tests**.

### Fixed

- **Windows builds**: Frontend now served correctly in installed apps.

## [0.4.0] - 2026-05-01

## [0.3.0] - 2026-05-01

- Non-technical README with download links and quick start guide.
- DESIGN.md with architecture, protocol, and API reference.
- App version shown in Settings → About.
- Energy flow diagram: Home on left, Grid on right.

## [0.2.0] - 2026-05-01

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

## [0.1.0] - 2025-05-01

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

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

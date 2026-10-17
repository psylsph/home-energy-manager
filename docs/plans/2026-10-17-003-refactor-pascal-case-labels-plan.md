---
title: "refactor: Make UI labels consistently Pascal Case"
type: refactor
status: complete
date: 2026-10-17
---

<!-- markdownlint-disable MD025 -->

# Make UI Labels Consistently Pascal Case

## Goal

Stuart prefers Pascal Case for UI labels. The frontend mixes Pascal Case, sentence case and lowercase across headings, buttons, table headers, form fields, chart series, select options, placeholders, `aria-label`s, tooltips, and status badges. The first pass fixed most of the list. This document now holds only the labels that are still open after a second, more detailed check on 2026-10-17.

This is a copy-only change. No behaviour, route, API field, or stored value changes. Where a label is used as a lookup key in code or tests, the key and the display text are kept separate (see Risks).

## Decisions and reasoning

- **What counts as a label.** A short phrase rendered as a name in the UI: heading, button, tab, table header, form field, chart or legend series, select option, placeholder, `aria-label`, short tooltip (about eight words or fewer), status badge, or empty-state line inside a panel. These get Pascal Case.
- **What does not.** Sentences and body copy, help text, toasts, and error or validation messages ("Failed to save settings", "URL copied!"). Code comments, CSV and data rows, units (`kWh`, `p/kWh`, `units`), identifiers, class names, SVG path data, and proper nouns (`ntfy`, `Telegram`, `Octopus`). These stay as they are. The line is drawn at standalone phrases versus full sentences; a message is a full sentence or a toast.
- **Empty-state lines count as labels.** The first pass changed "No Power History for This Range" and "No Data Available for This Period" to Pascal Case, so the other empty-state lines are changed to match.
- **Strict Pascal Case: every word capitalised.** The request said "Pascal Case", so small words ("of", "and", "for", "to", "per", "when", "below", "by", "vs") are capitalised too. Items that depend only on this rule are marked **(strict)** so they can be dropped if Stuart prefers Title Case. The first pass already applied strict casing, so dropping it now would mean reverting those too.
- **Units and symbols are left alone.** `°C`, `%`, `kWh`, `kWp`, `m³`, `W`, `p/kWh`, acronyms (`SOC`, `PV`, `CT`, `EPS`, `HEM`, `BMS`, `EV`, `API`), and proper nouns keep their existing form.
- **Parenthetical hints are listed separately**, at the end, as optional.
- **Lookup keys are not renamed.** Where a display string is also a key, only the rendered text changes, and tests that match on the old text are updated in the same change.

## Status

- Items fixed in the first pass have been removed from this list. They were checked against the code on 2026-10-17 and are Pascal Case now.
- Line numbers come from the 2026-10-17 scan. Some may have shifted after the first pass, so match by text if a number is off.
- Known inconsistency left by the first pass: "Grid Import vs Export" (PowerPage report) and "Solar generation vs home load" use a lowercase `vs`. Strict casing makes this "Vs". It's listed in the inventory below.

## Open inventory

Format: `file:line` — current text → proposed text.

### App.tsx

- 155 tooltip "Force a fresh connection to the inverter now" → "Force a Fresh Connection to the Inverter Now"

### components/AwaitingConnection.tsx

- 37, 78 button "Retry now" → "Retry Now"

### components/ErrorBoundary.tsx

- 94 button "Retry now" → "Retry Now"

### components/UpdateBanner.tsx

- 54 link "View release" → "View Release"
- 59 aria-label "Dismiss update notice" → "Dismiss Update Notice"
- 60 tooltip "Dismiss — I'll check the Settings page later" → "Dismiss — I'll Check the Settings Page Later" (strict)

### components/BatterySocChart.tsx and components/SolarPowerChart.tsx

- BatterySocChart 40 empty state "No SOC history yet today" → "No SOC History Yet Today"
- SolarPowerChart 43 empty state "No solar history yet today" → "No Solar History Yet Today"

### pages/LogsPage.tsx

- 236 empty state "No logs captured yet" → "No Logs Captured Yet"
- 236 empty state "No logs match the filter" → "No Logs Match the Filter" (strict)

### pages/HistoryPage.tsx

- 365 aria-label and 367 heading "Period totals" → "Period Totals"
- 927 aria-label "Select metric" → "Select Metric"
- 956 aria-label "Select time range" → "Select Time Range"
- 1017 aria-label "Select period date" → "Select Period Date"
- 1062 button "Export all" → "Export All"
- aria-label "Export all tabs as a single combined CSV" → "Export All Tabs as a Single Combined CSV" (strict)

### pages/ForecastPage.tsx

- 230 select option "Home use" → "Home Use"
- 889, 2054 series "If solar only" → "If Solar Only"
- 894, 2066 series "If charge enacted" → "If Charge Enacted"
- 904, 2079 series "With current schedule" → "With Current Schedule"
- 914, 2031 series "Planned (past)" → "Planned (Past)"
- 1674 series "Forecast (past)" → "Forecast (Past)"
- 1027 badge "preliminary — still calibrating" → "Preliminary — Still Calibrating"
- 1044 subtitle "with charge plan" → "With Charge Plan"
- 1138, 1141 button "Apply — schedule charge slot" → "Apply — Schedule Charge Slot"
- 1451 aria-label and 1454 heading "Export opportunity" → "Export Opportunity"

### pages/ControlPage.tsx

- 887 empty state "No active period" → "No Active Period"
- 965 "Preferred charge rate" → "Preferred Charge Rate"
- 966 "Recovery charge rate" → "Recovery Charge Rate"
- 1038 button "Check settings" → "Check Settings"
- 1306 button "Save slots" → "Save Slots"
- 1722 status "Lookup failed" → "Lookup Failed"
- 1941 "Setting lower limit…" → "Setting Lower Limit…"
- 1943 "Setting upper limit…" → "Setting Upper Limit…"
- 1945 "Setting full capacity…" → "Setting Full Capacity…"
- 3592 "Charge power limit" → "Charge Power Limit"
- 3604 "Discharge power limit" → "Discharge Power Limit"
- 4282 "Three-phase Charge Power Limit" → "Three-Phase Charge Power Limit"
- 4322 "Three-phase Discharge Power Limit" → "Three-Phase Discharge Power Limit"

### pages/SettingsPage.tsx

- 171 title "Remove window" → "Remove Window"
- 1357 fallback "Unknown serial" → "Unknown Serial"
- 1498 field "Listen address" → "Listen Address"
- 1522 button "Apply network settings" → "Apply Network Settings"
- 1539 button "Generate new key" → "Generate New Key"
- 1539 button "Generate API key" → "Generate API Key"
- 1693 field "Account number" → "Account Number"
- 1697 field "API key" → "API Key"
- 1717 field "Gas API unit" → "Gas API Unit"
- 1966 aria-label "Remove array" → "Remove Array"
- 2668 checkbox "Show average lines on History charts" → "Show Average Lines on History Charts" (strict)
- 2700 heading and 2708 aria-label "Grid CT meter" → "Grid CT Meter"
- 2907 aria-label "Check for new releases" → "Check for New Releases"

### pages/OctopusPage.tsx

- 372 button "Sync now" → "Sync Now"
- 380 status "Last sync" → "Last Sync"
- 381 status "Waiting for first sync" → "Waiting for First Sync" (strict)
- 383 status "Full history imported" → "Full History Imported"
- 384 status "Older history backfilling" → "Older History Backfilling"
- 652 series "Reported units" → "Reported Units"
- 672 series "Cumulative units" → "Cumulative Units"

### pages/PowerPage.tsx

- 682 report donut label "Used locally" → "Used Locally"
- 956 status "Waiting for data" → "Waiting for Data" (strict)
- 997 aria-label "Select time range" → "Select Time Range"
- 1048 aria-label "Select period date" → "Select Period Date"
- Report chart titles (template in the report HTML, around lines 655–698):
  - "Solar generation vs home load" → "Solar Generation Vs Home Load" (strict)
  - "Battery charge vs discharge" → "Battery Charge Vs Discharge" (strict)
  - "Grid Import vs Export" → "Grid Import Vs Export" (strict; already applied with lowercase `vs`)
  - "Grid balance" → "Grid Balance"
  - "Battery activity" → "Battery Activity"
  - "Estimated solar destination" → "Estimated Solar Destination"
  - "Estimated home source" → "Estimated Home Source"
  - "Best solar" → "Best Solar"
  - "Highest load" → "Highest Load"
  - "Highest import" → "Highest Import"
  - "Highest export" → "Highest Export"

### pages/InverterPage.tsx

- 71 empty state "No limit set" → "No Limit Set"
- 357, 369 status "On hold" → "On Hold"

### lib/ecoTimedExport.ts

- 532 "Eco — Temporarily overridden" → "Eco — Temporarily Overridden"
- 554 "Timed Export — Exporting now" → "Timed Export — Exporting Now"
- 556 "Timed Export — Blocked by Pause Discharge" → "Timed Export — Blocked By Pause Discharge" (strict)

### lib/batteryMode.ts

- 70 "armed · charging now" → "Armed · Charging Now"
- 85 "armed · exporting now" → "Armed · Exporting Now"
- 105 "armed · covering demand now" → "Armed · Covering Demand Now"

### lib/adaptiveCharge.ts

- 92 "Outside configured period" → "Outside Configured Period"
- 93 "Preferred rate active" → "Preferred Rate Active"
- 94 "Low-SOC recovery active" → "Low-SOC Recovery Active"
- 95 "Suspended by Auto Winter" → "Suspended By Auto Winter" (strict)
- 96 "Restoring manual rate" → "Restoring Manual Rate"

### lib/gridFault.ts and hooks/useGridOutageNotifications.ts

- gridFault.ts 51 "Grid power lost" → "Grid Power Lost"
- gridFault.ts 56 reason "no live grid AC reference" → "No Live Grid AC Reference"
- gridFault.ts 57 reason "grid fault" → "Grid Fault"
- useGridOutageNotifications.ts 58 "Grid power lost" → "Grid Power Lost"
- useGridOutageNotifications.ts 65 "Battery over temp warning" → "Battery Over Temp Warning"
- useGridOutageNotifications.ts 66 "Inverter fault" → "Inverter Fault"
- useGridOutageNotifications.ts 79 "Grid power restored" → "Grid Power Restored"
- useGridOutageNotifications.ts 84 "Battery temperature normal" → "Battery Temperature Normal"

### lib/planHistory.ts

- 299 "Before the cheap window" → "Before the Cheap Window" (strict)
- 300 "Auto-applied" → "Auto-Applied"
- 327 status "Plan unavailable" → "Plan Unavailable"

### lib/savingSessions.ts, lib/solarArrays.ts, lib/liveReadback.ts

- savingSessions.ts 21 fallback "time unknown" → "Time Unknown"
- solarArrays.ts 78 fallback "Solar array" → "Solar Array"
- liveReadback.ts 89 "Inverter Charge Rate: unknown" → "Inverter Charge Rate: Unknown"

### lib/historyRangeConfig.ts

- 8 "today" → "Today"
- 11 "month" → "Month"

### lib/octopusExport.ts (PDF table only)

- 568 PDF section heading "Octopus versus HEM and missing data" → "Octopus Versus HEM And Missing Data" (strict)
- 568 PDF column headers "Oct imp.", "HEM imp.", "Oct exp.", "HEM exp." → "Oct Imp.", "HEM Imp.", "Oct Exp.", "HEM Exp."

## Left alone by design

- **Sentence and body copy.** Examples: SummaryTiles tooltip sentences (lines 38–86), ForecastPage tooltips at 1221 and 1269, the energy-flow sentences in `lib/energyFlow.ts` (the "System is idle." and "the battery" / "the grid" phrase builders), "Uses the inverter's configured discharge-slot times.", "Press to stop Timed Export.", and the "Saving Sessions" and Octopus description paragraphs.
- **Toasts, errors and validation messages.** Examples: "URL copied!", "Save failed", "Failed to save settings", "Could not book the export.", "Tariff configuration is invalid", "Min SOC is required".
- **CSV data.** `lib/octopusExport.ts` CSV rows and headers (lines 112–186), including "Daily Comparison and Missing Data" and the "Gas unit" rows. These are data headers, not display labels.
- **Units and compact time labels.** `historyRangeConfig.ts` `1h`, `6h`, `7d`, `30d`, `1y` and the `planHistory.ts` `24h`, `3d`; `Rate (p/kWh)`; `units` in PDF axis labels.
- **Proper nouns and product names.** `ntfy Push Notifications`, `Telegram`, `Octopus`, `givenergy-modbus`, `Open-Meteo`.
- **Hints and examples.** `e.g. East roof`, `e.g. 192.168.1.50`, and the placeholder examples.
- **Code comments**, including the one at `SettingsPage.tsx` 2864.
- **Three-dot waits.** "Saving..." (`ControlPage.tsx` 1036) uses three dots rather than an ellipsis. This is punctuation, not casing, so it's left out. Mention if you want it changed.

## Parenthetical hints (optional, separate decision)

These are sub-labels, not labels. Leave them, or convert them with the same rule if Stuart wants full consistency:

- SettingsPage 1319 "(auto-detected)"
- SettingsPage 1508 "Allowed browser origins (CORS, comma-separated)"
- SettingsPage 2025 "Postcode (UK — resolved via api.postcodes.io)"
- SettingsPage 2180 "Bot Token (from @BotFather)" and 2189 "Chat ID (from @userinfobot)"
- SettingsPage 2248 "Server (optional, default: ntfy.sh)"
- SettingsPage 2269 "App API Token (from pushover.net/apps/build)"
- SettingsPage 2278 "User Key (from your Pushover account)"

## Method and limits

- The second pass was a scripted scan of frontend `.ts`/`.tsx` string literals and JSX text, then a manual read of each candidate in context. Class names, SVG path data, identifiers and comments were filtered out first.
- Multi-line JSX text that contains parentheses, `=` or `(` may have been skipped by the filter. Re-run the scan after the edits to confirm.
- Rust backend strings and labels built from variables at runtime are not covered.

## Approach

1. Confirm the strict-versus-Title decision for the **(strict)** items. If Title Case is preferred, drop those from the change and revert the strict items from the first pass.
2. Work file by file in the order of the inventory. Match by text rather than line number, since lines move.
3. Search the tests (`tests/`) and Playwright specs (`e2e/`) for each old string (`getByText`, `getByRole(..., { name })`, `getByLabelText`, `toHaveText`) and update them in the same change. Don't weaken assertions.
4. Check that no label is used as a `Record` key, switch value, or comparison. Where one is, change only the rendered text.
5. Re-run the scan from the Method section and confirm the open inventory is empty, apart from the items under "Left alone by design".

## Verification

- `npm run test` (Vitest) passes, with text-based assertions updated.
- `npm run lint` and `npm run build` pass.
- `npm run lint:md` passes for this plan file.
- Re-run the scan; the remaining lowercase candidates should all fall under "Left alone by design".
- The Playwright specs can't be run from this check. If they're changed, run `npm run test:e2e` before committing (it needs `npm run build` and `cargo build --release` first).

## Risks

- **Tests and e2e specs matching on text.** Renamed labels can break `getByText` queries in `tests/` and `e2e/`. Search before editing.
- **Duplicated strings.** Several labels repeat across files ("Grid power lost" in `gridFault.ts` and `useGridOutageNotifications.ts`; "Used Locally" in the report and the chart; "Export All" in the button and the aria-label). Change every occurrence together so the UI stays consistent.
- **Chart legends and tooltips** may read labels from data objects rather than JSX. Changes there need to be made at the data source.
- **Report HTML.** The PowerPage report and the Octopus PDF are generated strings. Check the rendered output after the change, not only the source.

## Out of scope

- Renaming routes, API fields, settings keys, or stored values.
- Changing sentence-case body copy, toasts, or error messages.
- CSV headers and data rows.
- Spelling or British/American variants beyond the typo fix already applied in ControlPage 3965.

## Outcome

The open inventory was applied by text, file by file, and every label in it is now Pascal Case. A re-scan of the source finds none of the listed labels in their old form.

- **Same text elsewhere.** To keep the UI consistent, the same replacement was applied where the label was shown a second time and the inventory had not listed it: Settings' own "View Release" button, the shared "Waiting for Data…" message (`awaitingConnection.ts`, shown on the Status page and others), and the `gridFault.ts` banner reasons "Inverter Fault" and "Battery Over Temp Warning".
- **Error messages built from a label.** Because the power-limit labels are reused to start the save-failure message, those messages now read "Charge Power Limit save failed: …" and "Discharge Power Limit save failed: …". The rest of each message is unchanged.
- **Nothing to do.** `historyRangeConfig.ts` already had `Today` and `Month` as labels. The lowercase `today` and `month` there are the `key:` values (lookup keys), which stay as they are.
- **Left as the plan says.** The optional parenthetical hints, sentence copy, toasts and errors, CSV data headers, units, proper nouns, comments, and the three-dot "Saving...".
- **Tests.** Assertions on the old text were updated, including a check of the generated Power report HTML. The Playwright `text=Waiting for data` selectors are case-insensitive substring matches and still work unchanged. The Playwright specs could not be run here, so run `npm run test:e2e` before committing.

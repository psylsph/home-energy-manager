# Plan — apply selected GivTCP 3.6.0-beta fixes to HEM

Source: <https://github.com/britkat1980/giv_tcp/releases> (`3.6.0-beta1/2/3`). Local GivTCP clone is on
`3.5`, so fixes were read from the `3.6.0-beta3` tag. The reference library clone `~/repos/givenergy-modbus`
is older (v2.1.2); where relevant, v2.13.0 sources were read from GitHub.

Scope agreed with owner: implement **items 1, 2 and 5** below. Item 3 (HV Gen 3 rate) was dropped after
cross-checking (see its section) and item 4 (Gateway 10-slot) is deliberately excluded — it contradicts HEM's
documented decision (issue #149) and needs hardware evidence. Line numbers below are approximate; they had
drifted from the code when the plan was cross-checked.

Status: items 1, 2 and 5 implemented on `master` (RED/GREEN commit pairs); item 3 dropped; item 4 not
attempted. Item 5's threshold is `>= 5` minutes, matching GivTCP's release note. The snapshot/`BatteryModule`
fields added for items 1 and 2 are `#[serde(default)]` and not surfaced in `src/lib/types.ts`.

| # | Fix | HEM status | Priority |
|---|---|---|---|
| 1 | Force Charge must also set slot 1's per-slot target SOC (beta3 #576) | Real bug | High |
| 2 | Battery lifetime Charge/Discharge totals from first battery BMS (beta3 #600) | Real data gap | Medium |
| 3 | HV Gen 3 battery rate 6 kW → 10 kW (beta2) | **Dropped** — superseded by beta3 #604 | — |
| 5 | Warn when inverter clock is >5 min out (beta3 #601) | Missing diagnostic | Low |

TDD throughout: RED test first, minimal fix, refactor; commit RED and GREEN separately. Run
`cargo test` + `cargo clippy` + `cargo fmt --check`, plus `npm run lint`/`npm run test` if frontend
types change. Tests must use a temp config dir (`GIVENERGY_LOCAL_CONFIG_DIR`) — see AGENTS.md.

---

## Item 1 — Force Charge sets slot 1's per-slot target SOC (HR 242)

**Why:** GivTCP beta3 #576 (`write.py:1350-1369`): on 10-slot inverters (Gen3, AIO, HV Gen3) the inverter
stops charging at the **lower of** the global charge target and slot 1's own target (HR 242). HEM writes
only the global target, so Force Charge can do nothing when SOC is already above slot 1's stored target.

**Files & change**

1. `src-tauri/src/inverter/poll.rs`
   - Add `#[serde(default)] pub charge_slot_1_target_soc: Option<u8>,` to `ForceChargeRevert`
     (struct at ~line 205-278). Default `None` keeps old persisted baselines deserialisable.
2. `src-tauri/src/inverter/decoder.rs`
   - In `decode_holding_240_299` (~1157-1301), also expose the raw HR 242 value unconditionally, e.g.
     store `snap.raw_charge_slot_1_target_soc: Option<u8>` (new field on `InverterSnapshot` in `model.rs`)
     when the extended block is polled. This gives an exact pre-force value even when slot 1 is disabled
     (GivTCP captures `Charge_Target_SOC_1` regardless of slot state). Only decode for
     `uses_extended_schedule_slots()` / `uses_three_phase_schedule_slots()` devices.
3. `src-tauri/src/server/api.rs`
   - `capture_force_charge_revert` (~1179-1243): set
     `charge_slot_1_target_soc: snap.raw_charge_slot_1_target_soc` for extended/three-phase devices,
     `None` otherwise.
   - `force_charge_at_unlocked` (~5519-5630): after `ControlCommand::ForceCharge` / `ThreePhaseForceCharge`
     is encoded, if the device uses per-slot targets (`uses_extended_schedule_slots()` or
     `uses_three_phase_schedule_slots()`), append `ControlCommand::SetChargeTargetSocSlot { slot: 1, soc: 100 }`
     (`encoder.rs:778-782`) so HR 242 = the same target. Do this on both the `minutes` and no-body paths.
   - `build_force_charge_stop_writes` (~1248-1377): for extended/three-phase devices, restore HR 242 from
     `revert.charge_slot_1_target_soc` (skip the write when `None`). Guard value to `4..=100`. Use
     `HR_CHARGE_TARGET_SOC_1` (already in `SAFE_WRITE_REGS`, `registers.rs:981`).
4. `src-tauri/src/inverter/encoder.rs`
   - No change needed to `ForceCharge` (device routing stays in api.rs), unless the RED test shows the
     register order matters. `SetChargeTargetSocSlot` already exists and validates 4-100.

5. **Restore confirmation (required).** `snapshot_matches_writes` (`api.rs`, ~1118) ends in `_ => false`, and
   `clear_confirmed_force_restorations` (`poll.rs`, ~1303) uses it to release Force Charge ownership. An HR 242
   restore write that it cannot verify would pin ownership forever and retry every 30 s. Add an `HR_CHARGE_TARGET_SOC_1`
   arm comparing against `snapshot.raw_charge_slot_1_target_soc` (the decoder only copies HR 242 into
   `charge_slots[0].target_soc` when slot 1 is enabled, so the raw field is needed for confirmation as well as capture).
6. **Struct-literal churn.** Every `ForceChargeRevert { .. }` literal needs the new field: `lib.rs` (~1707),
   `server/external_control.rs` (three), `poll.rs` tests (five), `api.rs` capture. Cover restart hydration and the
   external stop path (`external_commands.rs`) too.
7. **Stale-data caveat.** HR 240-299 can return stale data on old Gen3 firmware (ARM FW <= 302, see AGENTS.md), so the
   captured HR 242 may be stale there. The decoder already trusts it; accepted.

**Tests (RED first)**

- Encoder/register-sequence test: a force-charge start for an extended-slot device emits HR 242 = 100.
- `api.rs` start/stop round-trip test: capture pre-value (e.g. 30), start writes 100, stop restores 30.
- `ForceChargeRevert` serde test: a baseline JSON without the new field deserialises (`None`).
- Three-phase variant: HR 242 written and restored alongside HR 1111.

---

## Item 2 — Single-phase battery lifetime Charge/Discharge totals from first battery BMS

**Why:** GivTCP beta3 #600 reads the first battery BMS lifetime totals (`e_battery_charge_total` /
`e_battery_discharge_total`) for Gen1/Gen2/AC, falling back to the inverter alt1 registers (IR 180/181) when
the BMS reads 0, and omits the value rather than publishing 0. The current reference (v2.13.0
`givenergy_modbus/model/battery.py:85-86`) defines these at **IR(105) discharge / IR(106) charge**
(deci-kWh). HEM only fills `total_charge_kwh`/`total_discharge_kwh` for Gen1 via alt1
(`decoder.rs:920-937`); Gen2/Gen3/AC stay `0.0`, and IR 105/106 are never decoded.

**Files & change**

1. `src-tauri/src/inverter/model.rs`
   - `BatteryModule` (~741): add `charge_energy_total_kwh: f32` and `discharge_energy_total_kwh: f32`
     (default `0.0`), with a comment citing the v4.1.6 doc / givenergy-modbus battery.py IR(105/106).
2. `src-tauri/src/inverter/decoder.rs`
   - `decode_battery_block` (~1913-1995): decode IR 105 (discharge) and IR 106 (charge) as `u16 * 0.1` kWh.
     (Do **not** add these to the HV BCU path — HV uses IR 82-85.)
   - At the end of `decode_snapshot`, for single-phase LV models only
     (`!needs_three_phase_input_blocks() && !needs_gateway_input_blocks()`), if
     `battery_modules[0]` has non-zero BMS totals, assign `snap.total_charge_kwh` /
     `snap.total_discharge_kwh` from the BMS (BMS wins), leaving the existing Gen1 alt1 values as the
     fallback when the BMS is zero. Three-phase/HV/Gateway paths are untouched.
3. `src-tauri/src/inverter/sanitizer.rs`
   - These are lifetime totals: ensure they are carried/validated as monotonic (the existing
     `total_*_kwh` handling already treats lifetime counters as never-resetting — verify no daily
     midnight logic touches them).
   - **Dropout handling:** if the battery module read fails transiently the BMS-derived total would fall back
     to a lower/zero value and trip the monotonic check (flapping). Keep the previous total in that case, matching
     GivTCP, which omits the value rather than publishing 0.

**Tests (RED first)**

- LV battery block with IR 105/106 set → snapshot totals reflect BMS values.
- BMS totals 0 and Gen1 alt1 set → alt1 value retained (GivTCP fallback order).
- Three-phase/HV decoder with battery blocks present → IR 1394/1395 totals unchanged by item 2.
- Multi-battery: only battery #1 (index 0, device 0x32) feeds the snapshot total.
- Transient BMS dropout keeps the previous total and raises no monotonic violation.

Note: these fields are not currently shown in the UI (`src/lib/types.ts` only), so this is a data-correctness
fix; no frontend change planned.

---

## Item 3 — HV Gen 3 battery charge/discharge rate 6 kW → 10 kW — DROPPED

**Cross-check result:** the beta2 note (#603) said HV Gen 3 was capped at 6,000 W instead of 10,000 W, but
beta3 (#604) superseded it: the rate is now min(rated power, battery current limit (25 A on 8 kW, 30 A on 10 kW)
x 80 V per module x modules in one stack). A 3-module stack on an 8 kW inverter is 6,000 W, not 8,000 W, so a blanket
10 kW for `HybridHvGen3` would be wrong.

HEM already maps `0x8102 => 8000` and `0x8103 => 10000`; only the unmapped base code `0x8101` falls back to 6,000.
`max_battery_power_w` feeds only the forecast simulation and display (`forecast/mod.rs`), so impact is small.
Revisit only with an HV Gen 3 hardware capture, and then follow the #604 stack-size formula.

---

## Item 5 — Warn when the inverter clock is >5 min out

**Why:** GivTCP beta3 #601 logs a once-a-day warning when the inverter clock is >5 min out, because the
inverter resets Today counters at midnight by its own clock. HEM relies on `snapshot.inverter_time` for
time-driven automations (`state_machines::authoritative_minute_of_day`) but only warns when the clock is
*unavailable* (`poll.rs:3793-3803`), not when it is wrong.

**Files & change**

1. `src-tauri/src/inverter/state_machines.rs`
   - Add a pure, testable helper:
     `pub fn inverter_clock_skew_minutes(inverter_time: &str, host: chrono::NaiveDateTime) -> Option<i64>`
     that parses the `"YYYY-MM-DD HH:MM:SS"` format (same format `decode_system_time` emits) and returns the
     signed difference in whole minutes; `None` on parse failure.
2. `src-tauri/src/inverter/poll.rs`
   - Alongside `inverter_time_fallback_logged` (~2661), add `inverter_clock_skew_logged: bool`.
   - In the poll loop near ~3786 (where `host_now` is computed), if the skew is known and
     `skew.abs() >= 5` (GivTCP's release note says "5 minutes or more") and not yet logged this connection, emit one `tracing::warn!` naming both times and
     explaining that Today counters reset at the wrong time. Diagnostics only — no behaviour change.

**Tests (RED first)** in `state_machines.rs`:

- Exact match → `Some(0)`; `+5`/`-6` boundary; malformed string → `None`; date rollover across midnight.
- A separate pure `clock_skew_warning_due(skew, already_logged)` decides when to warn (threshold `>= 5` either way, once per connection).

---

## Out of scope (evaluated, no action)

- Hybrid vs AC-coupled Load Energy (beta3), Sync Time timezone (beta3), Force Export slot restore (beta1),
  Gateway/EMS battery power sign (beta1), rate rounding (beta2), midnight counter reset (beta1) — already
  correct/robust in HEM.
- Gateway 10-slot HR 240-299 (#577) — **investigate only**; conflicts with `model.rs:362-428` and issue #149.
- All HA-only release items (timeslot entities, log viewer, config page, REST restart, capability cache,
  combined-generation sensor, `givenergy_modbus_async` cache) — not applicable to HEM.

## Verification

1. `cd src-tauri && cargo fmt && cargo clippy && cargo test`
2. `npm run lint && npm run build && npm run test` (regenerate/verify `src/lib/types.ts` if any snapshot
   field names change — item 1's new field and item 2's `BatteryModule` fields are serde-serialised).
3. Item-specific: force-charge register-sequence, start/stop restore and restoration-confirmation tests; LV BMS
   totals tests; clock-skew helper tests.
4. Optional hardware/simulator check for item 1 (`npm run test:local` against
   `~/repos/givenergy-simulator`), since it is an inverter-behaviour claim.

---

## Appendix — full findings for future reference

Investigation date: 2026-10-03. Reference release: GivTCP `3.6.0-beta3` (commit `a84179b`, released
03 Oct 00:11), with `3.6.0-beta2` / `3.6.0-beta1` where relevant.

Local clones at time of writing were **out of date**: `~/repos/giv_tcp` was on `3.5` (`89156ca`) and
`~/repos/givenergy-modbus` on `v2.1.2` (`c81780b`). Fix descriptions were read from the `3.6.0-beta3` tag
on GitHub and v2.13.0 library sources were fetched separately. Re-sync the clones before any follow-up.

## Item 4 — Gateway 10-slot support (HR 240-299) — INVESTIGATE ONLY

**GivTCP beta3 #577:** the Gateway has the 10-slot block (HR 240-299) like other models. The library
doesn't read it there, so targets were never filled in; GivTCP now treats the Gateway as a 10-slot device,
so charge/discharge slots 3-10 and every slot's target SOC can be read and set. (GivTCP's own note also
says values left over from 3.5, published as 0, were rejected by HA as out of range.)

**HEM current behaviour (deliberate exclusion):**

- `src-tauri/src/inverter/model.rs:362-386` — `uses_three_phase_schedule_slots()` and
  `uses_extended_schedule_slots()` explicitly exclude `DeviceType::Gateway`, citing dewet22/givenergy-modbus
  `slot_map` (Gateway → `SINGLE_PHASE_SLOTS`) and the old GivTCP write routing ("gateway is not 3ph").
- `model.rs:397-428` — `max_charge_slots()` / `max_discharge_slots()` therefore return **2** for Gateway
  (the `_ => 2` arm), not 10.
- `model.rs:432-454` — `extra_poll_blocks()` never returns `EXTENDED_SLOTS_BLOCK` for Gateway, so
  HR 240-299 is not polled and per-slot targets are never decoded for it.
- `AGENTS.md` documents issue #149: the Gateway is **single-phase-class for control** — Quick Actions /
  schedules write the standard HR 94/95, 56/57, 96, 116 registers (forwarded to child AIOs), not the
  three-phase bank nor the EMS schedule — though it polls HR 2040-2075 for plant-level config read-back.

**Why it needs hardware evidence, not a blind change:** two authoritative references now disagree.
The upstream library's `slot_map` says Gateway uses single-phase slots, while GivTCP beta3 observed a
10-slot block on real Gateway hardware. Possibilities: the Gateway exposes HR 240-299 telemetry but does
not honour writes there; or it genuinely supports slots 3-10 and HEM under-serves it. Changing
`uses_extended_schedule_slots()`/slot counts for Gateway without a capture would risk reintroducing the
silent no-op that issue #149 fixed.

**If confirmed as a real 10-slot device, the work would be:**

- Add Gateway to `supports_gen3_extended()` (or a dedicated Gateway arm) so `max_*_slots()` = 10 and
  `EXTENDED_SLOTS_BLOCK` is polled; verify write routing still forwards the single-phase registers for
  slots 1-2.
- Decode/encode per-slot targets for the Gateway block without colliding with the EMS plant-level
  schedule (HR 2040-2071).
- Simulator/hardware test: read back slot 3-10 times and each per-slot target after a write.

## Out-of-scope GivTCP fixes — detailed rationale

Each was evaluated and found already correct or not applicable:

- **Hybrid vs AC-coupled Load Energy (beta3).** The bug was a single-phase hybrid using the AC-coupled
  "inverter output + PV" formula, double-counting PV. HEM derives single-phase `today_consumption_kwh`
  from a generic home-level energy balance (`decoder.rs:475-497`, `871-879`) and uses the inverter's
  native `e_load_today` for three-phase/HV (`decoder.rs:1808`) and Gateway (`decoder.rs:2329`). It never
  adds PV to inverter output, and HEM has no lifetime "Load Total" field to inflate — so the reported
  symptom (Load Total jumping by lifetime PV) cannot occur.
- **Sync Time timezone (beta3).** GivTCP's bug was writing the container clock (possibly UTC). HEM writes
  `chrono::Local` deliberately and documents why the inverter RTC is local time
  (`encoder.rs:730-755`, `1550-1560`; tests at `1550+`). Correct as-is.
- **Force Export slot restore (beta1).** GivTCP put the export window into charge slot 1 and left discharge
  slot 1 set. HEM's `ForceDischargeRevert` captures and restores `discharge_slot_1/2_start/end` explicitly
  (`poll.rs:330-370`; `api.rs:1474-1600`), so it cannot restore into the wrong direction.
- **Gateway/EMS battery power sign (beta1).** Already negated on decode
  (`decoder.rs:2373-2381`, `snap.battery_power = -p_aio_total`), matching `GivTCP/read.py:1556` and
  issue #78. The gateway grid-power balance and per-AIO reads build on that corrected sign (`decoder.rs:448-459`).
- **Battery rate rounding to nearest percent (beta2).** The bug was a W→percent conversion rounding below
  max (2976 vs 3000 W). HEM's control API takes a percentage directly
  (`api.rs:4967-5055`, `encoder.rs:402-430`); W↔% is display-only (`ControlPage.tsx:2989-2993`). No
  write-path rounding exists. N/A. (The *cap* value itself — item 3 — is a separate matter.)
- **Today counters stuck at midnight (beta1).** HEM has deeper protection: the sanitizer gates daily-counter
  resets on the inverter's own date/time (`sanitizer.rs:1498-1600`) and accepts an inverter-midnight reset
  even when host-local time disagrees (`sanitizer.rs:4534-4576`, `4666-4726`). More robust than GivTCP's fix.
- **Register-read failure → last-good republish (beta1).** HEM carries forward optional blocks
  (`carry_forward_optional_block_values`) and sanitizer delta checks; not re-verified field-by-field in this
  pass. Worth a separate audit if a required-block failure ever flashes zeros.
- **Force Charge / Export revert when settings unreadable (beta1).** HEM refuses to start without a
  verified inverter identity and stores `Option` fields for partial captures
  (`api.rs:5536-5578`, `1220-1243`), matching the fix's intent.
- **Enable Discharge sets reserve (beta1).** HEM has no standalone "Enable Discharge" control that silently
  did nothing; reserve is set via `/api/control/reserve` (`api.rs:4936-4965`).
- **`givenergy_modbus_async` cache / REST worker restart / config-page model cache / combined-generation
  sensor / timeslot entities / log viewer (betas).** All Home-Assistant/Docker/Python-stack specific;
  not applicable to HEM's Rust/Axum/Tauri architecture.

## Key reference locations used

- GivTCP force-charge fix (item 1): `GivTCP/write.py` @ `3.6.0-beta3`, `forceCharge()` lines 1330-1404
  (capture `slot1TargetSOC` at 1350-1352; write `slotTargetSOC(device,"charge",1,100)` at 1365-1369).
- GivTCP clock warning (item 5): `GivTCP/read.py` @ `3.6.0-beta3` lines ~100-105.
- GivTCP battery totals (item 2): `GivTCP/read.py` lines 107-115 (`batteryTotals`) and 1277-1287.
- Library battery totals (item 2): givenergy-modbus v2.13.0 `givenergy_modbus/model/battery.py:76,85-86`
  — IR(105) discharge total, IR(106) charge total, deci-kWh ("Battery discharge/charge energy total",
  v4.1.6 doc).
- Library battery power tables (item 3): v2.13.0 `givenergy_modbus/model/inverter.py`
  — `_BATTERY_ENERGY_SOURCE` (~729), `e_battery_charge_total` property (~812), `_DTC_BATPOWER` (164-173,
  **no `8101`**), `_DTC_MAXPOWER` (149-151, `8101`=6000), `battery_max_power` property (~1204).
- HEM snapshot fields: `total_charge_kwh` / `total_discharge_kwh` (`model.rs:1042-1050`), currently
  exposed in `src/lib/types.ts:45-46` but unused by the UI (so item 2 is data-correctness, not a visible
  regression today).

## Clones to re-sync before follow-up

- `~/repos/giv_tcp` — currently `3.5`; fetch the `3.6.0-beta3` tag to read fixes in place.
- `~/repos/givenergy-modbus` — currently `v2.1.2`; item 2's IR 105/106 and item 3's `_DTC_BATPOWER` only
  exist in newer releases (v2.13.0 used here).

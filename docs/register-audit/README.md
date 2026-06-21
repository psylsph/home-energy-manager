# Register Audit — HEM vs GivTCP vs givenergy-modbus Reference

Deep-dive comparison of every GivEnergy Modbus register across three codebases.
Generated 2026-06-21. Investigation/recording only — no code changes were made.

## Sources

| Label | Path | Role |
|-------|------|------|
| **HEM** | `/home/stuart/repos/givenergy-local/src-tauri/src/` | The app under audit |
| **GivTCP** | `~/repos/giv_tcp/GivTCP/` | Authoritative for register semantics (britkat1980/giv_tcp) |
| **Reference** | `~/repos/givenergy-modbus/givenergy_modbus/` | Reference library (dewet22/givenergy-modbus) |

## Detailed reports

| File | Scope |
|------|-------|
| [single-phase.md](single-phase.md) | IR 0–59, IR 180–183; HR 0–59, 60–119, 120–179, 180–239, 240–299, 300–359, 554–573 |
| [three-phase-gateway-ems.md](three-phase-gateway-ems.md) | HR 1000–1124, IR 1000–1413, IR 1600–1859 (Gateway), HR 2040–2071 (EMS) |
| [battery-meter.md](battery-meter.md) | LV Battery IR 60–119, HV BCU/BMU IR 60–119, Meter IR 60–89 |

---

## Cross-verified key findings

Each finding below was independently verified by direct source reads, not just
the subagent reports. Findings are classified by where the bug lives.

### Bugs in HEM

#### H1. `decode_holding_1000_1079` is a no-op — HR 1005 and HR 1078 discarded

`decoder.rs:995` — the function signature is `fn decode_holding_1000_1079(_data,
_snap, _raw)` (all params underscore-prefixed = unused). The body does nothing.
HR 1005 (REAL_TIME_CONTROL, three-phase mirror of HR 166 ENABLE_RTC) and HR 1078
(BATTERY_POWER_CUTOFF / battery reserve percent) are polled via
`THREE_PHASE_HIGH_CONFIG_BLOCK` but the data is thrown away.

- HEM: `decoder.rs:995-999` (empty body)
- Reference: `inverter_threephase.py:269` defines `battery_power_cutoff` at
  `HR(1078)`; HR 1005 is not in the LUT but is in `SAFE_WRITE_REGS`
- GivTCP: `threephase.py:~105` defines `battery_power_cutoff` at `HR(1078)`
- Impact: HEM cannot display or restore these settings on three-phase models.
  They are writable (in `SAFE_WRITE_REGS` at `registers.rs:497-498`) but never
  read back.

#### H2. `IR_TODAY_CONSUMPTION` constant name is a stale misnomer

`registers.rs:138` names IR(35) `IR_TODAY_CONSUMPTION`. The reference library
renamed this to `e_ac_charge_today` (PR #174 — sentinel cross-correlation
confirmed it's AC charge from grid, NOT house consumption). HEM's decoder at
`decoder.rs:599` correctly decodes it as `today_ac_charge_kwh` with the right
semantics — only the constant name is wrong.

- HEM constant: `registers.rs:138` (`IR_TODAY_CONSUMPTION: u16 = 35`)
- HEM decode: `decoder.rs:599` (`snap.today_ac_charge_kwh = get_reg(data, 35) as f32 * 0.1`)
- Reference: `inverter.py:659` (`"e_ac_charge_today": Def(C.deci, None, IR(35))`)
- Impact: Cosmetic/naming only. The decode is correct.

#### H3. HR 1124 (battery_maintenance_mode) not decoded on three-phase

The `THREE_PHASE_CONFIG_BLOCK` reads 45 registers (HR 1080–1124), so HR 1124 IS
polled. But `decode_holding_1080_1124` at `decoder.rs:1001` stops before 1124 — no
constant, no decode, not in `SAFE_WRITE_REGS`.

- Reference: `inverter_threephase.py:309` (`"battery_maintenance_mode": Def(C.uint16, BatteryMaintenance, HR(1124))`)
- GivTCP: `threephase.py:139` (same)
- Impact: HEM cannot read or control three-phase battery maintenance mode.

#### H4. AIO models: HR 313/314 may clobber HR 111/112 charge/discharge limits

`decode_holding_300_359` at `decoder.rs:963-964` unconditionally writes
`charge_rate`/`discharge_rate` from HR(313/314). For pure DC hybrids this is a
non-issue (AC_CONFIG_BLOCK isn't polled). But AIO models poll both
`EXTENDED_SLOTS_BLOCK` and `AC_CONFIG_BLOCK`, so if an AIO uses HR(111/112) for DC
battery limits AND also has AC limits at 313/314, the AC block decode overwrites
the DC values. The defensive skip at `decoder.rs:797-800` only protects
ACCoupled/ACCoupledMk2 — not AIO.

- HEM guard (insufficient): `decoder.rs:797-800` (skips 111/112 only for `ACCoupled | ACCoupledMk2`)
- HEM clobber: `decoder.rs:963-964` (unconditional write from 313/314)
- Impact: Possible wrong charge/discharge rate display on AIO models. Needs
  confirmation against a real AIO device profile.

#### H5. LV battery cap_design2 (IR 101-102) not decoded

HEM decodes `cap_design` from IR(86-87) at `decoder.rs:1393` and stores it in
`design_capacity_ah`. But the secondary `cap_design2` register at IR(101-102)
(centi-Ah uint32) is never decoded — it isn't in the `BatteryModule` struct.
Both references define it.

- HEM: `decoder.rs:1393` decodes 86-87 only; no 101-102 decode
- Reference: `battery.py:68` (`"cap_design2": Def(DT.uint32, DT.centi, IR(101), IR(102))`)
- GivTCP: `battery.py` defines the same
- Impact: HEM can't surface calibration drift between cap_design and cap_design2.

#### H6. Meter decode skips IR(66) i_ln (neutral-line current)

`decode_meter_data` reads IR(63-65) and IR(67) but skips IR(66). Both reference
and GivTCP include it.

- HEM skip: `decoder.rs:1288` (jumps from offset 7 to offset 9)
- Reference: `meter.py:36` (`"i_ln": Def(C.centi, None, IR(66))`)
- Impact: Minor — neutral current is informational, not used in calculations.

#### H7. EMS HR 2040 comment groups plant_status with discharge slots

`registers.rs:499` comment says "EMS plant-level control / discharge slots" and
lists `2040, 2044, 2045, ... 2052` together. But HR(2040) is `plant_status`
(master plant enable/disable toggle), not a discharge slot. The discharge slots
start at HR(2044). The SAFE_WRITE inclusion is correct (HR 2040 IS writable),
but the comment is misleading.

- HEM: `registers.rs:499-500`
- Reference: `ems.py:59` (`"plant_status": Def(C.uint16, Status, HR(2040))`)
- Impact: Cosmetic — comment only. No functional bug.

### Bugs in GivTCP (not HEM)

#### G1. HR(180) type mismatch — should be IR(180)

GivTCP `baseinverter.py:290` defines `e_battery_discharge_total_2` as `HR(180)`
(holding register). The reference (`inverter.py:702`) and HEM
(`registers.rs:67-72`, Input type) both use `IR(180)`. GivTCP line 291 correctly
uses `IR(181)` for the adjacent register — so line 290 is a copy-paste typo.

- GivTCP: `baseinverter.py:290` (`Def(C.deci, None, HR(180))` ← wrong)
- Reference: `inverter.py:702` (`Def(C.deci, None, IR(180))`)
- HEM: `registers.rs:67-72` (`RegisterType::Input`, start=180)
- Impact: GivTCP would read the wrong register type for this field. HEM is correct.

#### G2. Gateway V2 uint32 byte order not handled

GivTCP's gateway model always uses V1 byte order (high-register-first) for uint32
energy totals. V2 firmware (GA000010+) swaps the register pair order. HEM handles
both via `gw_u32()` (`decoder.rs:1619-1627`).

- GivTCP: `gateway.py:53-57` (hardcoded V1 order)
- Reference: `gateway.py:141-156` (V2 swaps all totals)
- HEM: `decoder.rs:1619-1627` (variant-aware)
- Impact: GivTCP gives garbage energy totals on GA000010+ firmware. HEM is correct.

#### G3. HV BCU IR(79) battery_power treated as unsigned

GivTCP (`hvbcu.py:42`) and the reference (`hv_bcu.py:39`) both apply `C.milli`
(÷1000) to IR(79) without signed conversion. Negative power (discharge) wraps to
>32 kW. HEM uses `signed()` cast (`decoder.rs:1504`), which is correct.

- GivTCP: `hvbcu.py:42` (`DT.milli`, unsigned)
- Reference: `hv_bcu.py:39` (`C.milli`, unsigned)
- HEM: `decoder.rs:1504` (`signed()`, correct)
- Impact: Reference/GivTCP have a sign bug on HV battery discharge. HEM is correct.

#### G4. Three-phase system_mode uses C.bool — truncates WorkMode enum

GivTCP `threephase.py:164` defines `system_mode` with `C.bool` converter at
IR(1075). The WorkMode enum has values 0-4 (Wait/Normal/Check/Discharge/Charge
etc.), but `C.bool` truncates to 0-1. The reference uses `C.uint16`.

- GivTCP: `threephase.py:164` (`Def(C.bool, SystemMode, IR(1075))` ← wrong)
- Reference: `inverter_threephase.py:341` (`Def(C.uint16, None, IR(1075))`)
- Impact: GivTCP three-phase system mode decodes incorrectly for modes > 1.

#### G5. Duplicate battery_type — HR(1012) nibble overwritten by HR(1080)

GivTCP defines `battery_type` twice in the three-phase LUT: first as a nibble
extraction from HR(1012) (`threephase.py:45`), then as a full register at
HR(1080) (`threephase.py:106`). Dict update semantics mean the second wins — the
HR(1012) definition is dead code. The reference explicitly does NOT decode
HR(1012) for battery_type, routing it only through HR(1080).

- GivTCP dead code: `threephase.py:45` (overwritten by `:106`)
- Reference: `inverter_threephase.py:206` (HR 1012 intentionally not decoded)
- Impact: No functional bug (HR 1080 wins), but the dead nibble definition is
  misleading and could cause confusion during maintenance.

### Naming divergences (no functional bug, but confusing)

#### N1. IR(35) three-way naming disagreement

| Codebase | Name | Correct? |
|----------|------|----------|
| HEM constant | `IR_TODAY_CONSUMPTION` | ❌ stale name |
| HEM decode field | `today_ac_charge_kwh` | ✅ correct |
| GivTCP | `e_inverter_in_day` | ❌ wrong (inverter-in, not AC charge) |
| Reference | `e_ac_charge_today` | ✅ correct (renamed in #174) |

#### N2. IR(44)/IR(45-46) naming

Reference renamed these from `e_inverter_out_*` to `e_pv_generation_*` (confirmed
PV generation, not inverter AC output). GivTCP still uses the old names. HEM
doesn't decode these single-phase registers at all (uses the daily PV energy from
IR(17)/IR(19) instead).

### Confirmed correct (points in HEM's favor)

| Area | Status | Evidence |
|------|--------|----------|
| IR(52) battery power sign (+ = discharge) | ✅ All three agree | `decoder.rs:457`, `inverter.py:689`, GivTCP read.py |
| IR(30) grid power sign (+ = export) | ✅ All three agree | `decoder.rs:466`, `inverter.py:649`, GivTCP read.py:860 |
| IR(51) battery current sign (+ = discharge) | ✅ All three agree | `decoder.rs:461`, `inverter.py:688` |
| HHMM disabled sentinel (value 60) | ✅ All three agree | `registers.rs:762`, `register.py:Converter.timeslot` |
| Gateway V1/V2 byte order | ✅ HEM handles both | `decoder.rs:1619-1627` (GivTCP does not) |
| HV battery power sign | ✅ HEM correct | `decoder.rs:1504` uses `signed()` |
| HV capacity per-module handling | ✅ HEM correct | `decoder.rs:1473-1476` multiplies by module count |
| EMS slot layout HR 2040-2071 | ✅ Matches reference slot_map | `registers.rs:499-503`, `slot_map.py:EMS_SLOTS` |
| Three-phase battery_power = discharge - charge | ✅ All three agree | `decoder.rs:1141-1142` |
| Gateway p_aio_total negation | ✅ All three agree | `decoder.rs:1592`, GivTCP read.py:1556 |

---

## Methodology

Three parallel subagents performed register-by-register comparison, each covering
one register family:

1. Single-phase inverter (IR 0-59, HR 0-299, HR 300-359, HR 554-573, IR 180-183)
2. Three-phase / Gateway / EMS (HR 1000-1124, IR 1000-1413, IR 1600-1859, HR 2040-2071)
3. Battery (LV + HV) and Meter (IR 60-119, IR 60-89)

Each subagent read the actual source files across all three codebases and cited
file:line evidence for every claim. The orchestrator then cross-verified the
highest-impact findings by direct source reads before including them here.

Files not modified. No code changes. This is a reference document.

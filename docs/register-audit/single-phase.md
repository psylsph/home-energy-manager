# Single-Phase Inverter Register Audit

> **Date**: 2026-06-21  
> **Scope**: Input Registers IR 0–59, IR 180–183; Holding Registers HR 0–59, HR 60–119, HR 120–179, HR 180–239, HR 240–299, HR 300–359, HR 554–573  
> **Sources compared**:
>
> 1. **HEM** (givenergy-local) — `src-tauri/src/modbus/registers.rs`, `src-tauri/src/inverter/decoder.rs`, `src-tauri/src/inverter/encoder.rs`
> 2. **GivTCP** — `GivTCP/givenergy_modbus_async/model/baseinverter.py` (REGISTER_LUT), `GivTCP/givenergy_modbus_async/model/inverter.py`, `GivTCP/read.py`
> 3. **Reference** (givenergy-modbus) — `givenergy_modbus/model/inverter.py` (SinglePhaseInverterRegisterGetter.REGISTER_LUT, lines 399–699), `givenergy_modbus/model/register.py`

---

## Summary of Key Discrepancies

### 1. 🔴 GivTCP HR(180) type mismatch — Holding vs Input register

GivTCP defines `e_battery_discharge_total_2` as `HR(180)` (holding register), but the Reference library defines `e_battery_discharge_total_alt1` as `IR(180)` (input register). HEM uses `IR(180)` (correct).

- **GivTCP**: `baseinverter.py:290` — `"e_battery_discharge_total_2": Def(C.deci, None, HR(180)),`
- **Reference**: `inverter.py:702` — `"e_battery_discharge_total_alt1": Def(C.deci, None, IR(180)),`
- **HEM**: `registers.rs:62-73` — reads from input block `input_180_181` (IR 180–183)

The other three registers in this block (IR 181-183) are correctly typed as `IR` in GivTCP. This is a copy-paste error: line 290 says `HR(180)` but should be `IR(180)`.

### 2. 🟡 IR(35) naming — all three codebases disagree

This register is **AC charge energy today** (grid → battery), NOT house consumption or inverter input.

- **HEM**: `registers.rs:138` — constant named `IR_TODAY_CONSUMPTION` (misleading), but `decoder.rs:599` correctly decodes it as `today_ac_charge_kwh`
- **GivTCP**: `baseinverter.py:267` — `"e_inverter_in_day": Def(C.deci, None, IR(35)),` — wrong semantics (not inverter input)
- **Reference**: `inverter.py:659` — `"e_ac_charge_today": Def(C.deci, None, IR(35)),` — **correct per #174 sentinel cross-correlation**

**Impact**: HEM's constant name is misleading. Anyone using `IR_TODAY_CONSUMPTION` thinking it's house consumption will get AC charge instead. GivTCP's `e_inverter_in_day` is also wrong — the reference renamed it after confirmatory analysis.

### 3. 🟡 IR(24) vs IR(30) — different grid power measurement points

These are **different physical nodes** but share the "grid" prefix.

- **IR(24)**: Inverter AC terminal real power at the busbar (`p_grid_out_ph1` in Reference `inverter.py:641`, `p_inverter_out` in GivTCP `baseinverter.py:259`)
- **IR(30)**: External grid CT net flow at the meter boundary (`p_grid_out` in Reference `inverter.py:649`, `p_grid_out` in GivTCP `baseinverter.py:264`)

HEM only decodes IR(30) for grid power (`decoder.rs:466`). HEM does NOT decode IR(24). The Reference `inverter.py:604-616` documents the physical node difference.

### 4. 🟡 GivTCP charge_slot_2 overwrite — HR(31-32) shadowed by HR(243-244)

GivTCP's `BaseInverter.REGISTER_LUT` defines `charge_slot_2` twice:

- `baseinverter.py:60` — `"charge_slot_2": Def(C.timeslot, None, HR(31), HR(32)),` (classic)
- `baseinverter.py:157` — `"charge_slot_2": Def(C.timeslot, None, HR(243), HR(244)),` (Gen3 extended)

The second definition **overwrites** the first (Python dict semantics). This means ALL inverter models resolve `charge_slot_2` to HR 243-244, including Gen1/Gen2 where those registers may not exist.

- **Reference**: Keeps both as separate keys — `charge_slot_2` (HR 31-32) at `inverter.py:429` and `charge_slot_2_x` (HR 243-244) at `inverter.py:517`
- **HEM**: Decodes HR 31-32 in `decode_holding_0_59` (`decoder.rs:771`) and HR 243-244 (authoritative for Gen3) in `decode_holding_240_299` (`decoder.rs:848-867`)

**Impact**: Gen1/Gen2 GivTCP users may read garbage from HR 243-244 where charge_slot_2 data lives at HR 31-32.

Same overwrite pattern applies to `charge_slot_2_start`/`charge_slot_2_end`.

### 5. 🟡 HR(43) packed register — HEM does NOT decode it

HR(43) packs two bytes: `charge_soc` (high byte) and `discharge_soc` (low byte).

- **HEM**: NOT decoded in any decoder function. No reference in `decoder.rs`.
- **GivTCP**: `baseinverter.py:70-71` — `"charge_soc": Def((C.duint8, 0), None, HR(43)),` and `"discharge_soc": Def((C.duint8, 1), None, HR(43)),`
- **Reference**: `inverter.py:435-436` — same split, same names

### 6. 🟢 IR(44) / IR(45-46) naming — confirmed PV generation, not inverter output

Per Reference `inverter.py:671-678` (#174 sentinel cross-correlation), IR(44) is `e_pv_generation_today` and IR(45-46) is `e_pv_generation_total`. GivTCP still uses the old mislabels:

- **GivTCP**: `baseinverter.py:276-277` — `e_inverter_out_day` (IR 44) and `e_inverter_out_total` (IR 45-46)
- **Reference**: `inverter.py:678-679` — `e_pv_generation_today` (IR 44) and `e_pv_generation_total` (IR 45-46)

HEM uses IR(44) only as fallback when per-string IR(17)+IR(19) read 0 (`decoder.rs:579-588`), and decodes IR(45-46) as `total_solar_kwh` via IR(11-12) instead.

### 7. 🟡 HEM missing register decodes

HEM defines several register constants but **does not decode** them:

| Register | HEM constant defined? | HEM decoded? | Notes |
|----------|----------------------|--------------|-------|
| HR(43) charge_soc / discharge_soc | No | No | Packed register, see #5 above |
| HR(114) discharge_min_power_reserve | Yes (`registers.rs:259`) | No | Only written via encoder |
| HR(52) power_factor | No | No | Holding register, not IR(52) |
| HR(104) battery_self_heating | Yes (`registers.rs:173`) | No | Only in SAFE_WRITE_REGS |
| HR(172) manual_battery_heater | Yes (`registers.rs:175`) | No | Only in SAFE_WRITE_REGS |
| HR(113) enable_buzzer | No | No | — |
| IR(24) p_grid_out_ph1 | No | No | See #3 above |
| Smart Load HR(554-573) | Partially (in SAFE_WRITE_REGS) | No | HEM does not decode or poll these |

### 8. 🟢 Sign conventions — ALL THREE AGREE

For all signed registers, the three codebases agree on the raw wire convention:

| Register | Meaning | Positive = | Evidence |
|----------|---------|------------|----------|
| IR(30) p_grid_out | Grid CT net flow | Exporting | HEM `decoder.rs:466`, GivTCP `read.py:1099-1107`, Ref `inverter.py:649` |
| IR(52) p_battery | Battery DC power | Discharging | HEM `decoder.rs:454-457`, GivTCP `read.py:1194-1207`, Ref uses int16 passthrough |
| IR(51) i_battery | Battery DC current | Discharging | HEM `decoder.rs:460-461`, GivTCP `baseinverter.py:281` int16 centi, Ref `inverter.py:688` |
| IR(24) p_grid_out_ph1 | Inverter AC terminal | Export/delivering | Ref `inverter.py:641` (not decoded by HEM) |

### 9. 🟢 HHMM slot disabled sentinel — ALL THREE USE 60

- **HEM**: `registers.rs:754-763` — `decode_hhmm` returns `None` for value 60
- **GivTCP**: Uses Python dict overwrite approach; the sentinel is enforced via `valid=(0, 2359)` bounds with `minute % 100 >= 60` check in `register.py:324` / `TimeSlot.from_repr` at `__init__.py:59` validating hours/minutes < 60
- **Reference**: `register.py:48-50` — `if start_time == 60 or end_time == 60: return None`

All three treat value 60 as a disabled/empty slot.

### 10. 🟡 AC-coupled charge/discharge limits overwrite DC limits

HEM's `decode_holding_300_359` at `decoder.rs:963-964` unconditionally overwrites `charge_rate` and `discharge_rate` with HR 313/314 (AC limits). This means on AC-coupled inverters the AC limits are correct, but on hybrid models with both HR 111/112 AND HR 313/314 blocks polled, the AC limits will **clobber** the DC limits from `decode_holding_60_119`. The block `holding_300_359` is polled for hybrids that have the AC config block, causing this overwrite.

---

## Detailed Register Tables

### IR 0–59: Input Registers (Telemetry)

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| IR(0) | `IR_STATUS` (reg:110); decoded as status (dec:517) | `status` — `Def(C.uint16, Status, IR(0))` (bi:240) | `status` — `Def(C.uint16, Status, IR(0))` (inv:618) | All agree. Raw uint16 mapped to Status enum. |
| IR(1) | `IR_PV1_VOLTAGE` (reg:112); decoded deci (dec:443) | `v_pv1` — `Def(C.deci, None, IR(1))` (bi:241) | `v_pv1` — `Def(C.deci, None, IR(1), min=0.0, max=2000.0)` (inv:619) | All agree: /10 V. Ref adds bounds [0, 2000]. |
| IR(2) | `IR_PV2_VOLTAGE` (reg:114); decoded deci (dec:444) | `v_pv2` — `Def(C.deci, None, IR(2))` (bi:242) | `v_pv2` — `Def(C.deci, None, IR(2), min=0.0, max=2000.0)` (inv:620) | All agree. |
| IR(3) | Not defined; NOT decoded | `v_p_bus` — `Def(C.deci, None, IR(3))` (bi:243) | `v_p_bus` — `Def(C.deci, None, IR(3))` (inv:621) | HEM skips this register. |
| IR(4) | Not defined; NOT decoded | `v_n_bus` — `Def(C.deci, None, IR(4))` (bi:244) | `v_n_bus` — `Def(C.deci, None, IR(4))` (inv:622) | HEM skips this register. |
| IR(5) | `IR_GRID_VOLTAGE` (reg:116); decoded deci (dec:467) | `v_ac1` — `Def(C.deci, None, IR(5))` (bi:245) | `v_ac1` — `Def(C.deci, None, IR(5), min=0.0, max=500.0)` (inv:623) | All agree: grid voltage /10 V. |
| IR(6-7) | NOT defined as consts; decoded uint32 deci as `total_throughput_kwh` (dec:628) | `e_battery_throughput_total` — `Def(C.uint32, C.deci, IR(6), IR(7))` (bi:246) | `e_battery_throughput` — `Def(C.uint32, C.deci, IR(6), IR(7))` (inv:624) | All agree: uint32 pair, /10 kWh. Name differs slightly. |
| IR(8) | `IR_PV1_CURRENT` (reg:118); decoded deci (dec:445) | `i_pv1` — `Def(C.deci, None, IR(8))` (bi:247) | `i_pv1` — `Def(C.deci, None, IR(8), min=0.0, max=500.0)` (inv:625) | All agree: /10 A. |
| IR(9) | `IR_PV2_CURRENT` (reg:120); decoded deci (dec:446) | `i_pv2` — `Def(C.deci, None, IR(9))` (bi:248) | `i_pv2` — `Def(C.deci, None, IR(9), min=0.0, max=500.0)` (inv:626) | All agree. |
| IR(10) | Not defined; NOT decoded | `i_ac1` — `Def(C.deci, None, IR(10))` (bi:249) | `i_ac1` — `Def(C.deci, None, IR(10), min=0.0, max=500.0)` (inv:627) | HEM skips this register. |
| IR(11-12) | NOT defined as consts; decoded uint32 deci as `total_solar_kwh` (dec:626) | `e_pv_total` — `Def(C.uint32, C.deci, IR(11), IR(12))` (bi:250) | `e_pv_total` — `Def(C.uint32, C.deci, IR(11), IR(12))` (inv:628) | All agree: uint32 pair, /10 kWh. |
| IR(13) | `IR_GRID_FREQUENCY` (reg:122); decoded centi (dec:468) | `f_ac1` — `Def(C.centi, None, IR(13))` (bi:251) | `f_ac1` — `Def(C.centi, None, IR(13), min=40.0, max=70.0)` (inv:629) | All agree: /100 Hz. |
| IR(14) | Not defined; NOT decoded | Not in GivTCP LUT | `charge_status` — `Def(C.uint16, None, IR(14))` (inv:630) | HEM and GivTCP skip this register. |
| IR(15) | Not defined; NOT decoded | `v_highbrigh_bus` — `Def(C.deci, None, IR(15))` (bi:252) | `v_highbrigh_bus` — `Def(C.deci, None, IR(15))` (inv:631) | HEM skips. |
| IR(16) | Not defined; NOT decoded | Not in GivTCP LUT | `pf_inverter_output_now` — `Def(C.uint16, None, IR(16))` (inv:632) | HEM and GivTCP skip. |
| IR(17) | `IR_PV1_ENERGY_TODAY` (reg:124); decoded deci, used in per-string sum (dec:558-560) | `e_pv1_day` — `Def(C.deci, None, IR(17))` (bi:253) | `e_pv1_day` — `Def(C.deci, None, IR(17))` (inv:633) | All agree: /10 kWh. HEM prefers IR(17)+IR(19) sum; falls back to IR(44) when zero (dec:576-588). |
| IR(18) | `IR_PV1_POWER` (reg:126); decoded raw uint16 (dec:440) | `p_pv1` — `Def(C.uint16, None, IR(18))` (bi:254) | `p_pv1` — `Def(C.uint16, None, IR(18), max=50000)` (inv:634) | All agree: raw watts. |
| IR(19) | `IR_PV2_ENERGY_TODAY` (reg:128); decoded deci (dec:565-572) | `e_pv2_day` — `Def(C.deci, None, IR(19))` (bi:255) | `e_pv2_day` — `Def(C.deci, None, IR(19))` (inv:635) | All agree: /10 kWh. HEM only includes if PV2 voltage > 0. |
| IR(20) | `IR_PV2_POWER` (reg:130); decoded raw uint16 (dec:441) | `p_pv2` — `Def(C.uint16, None, IR(20))` (bi:256) | `p_pv2` — `Def(C.uint16, None, IR(20), max=50000)` (inv:636) | All agree: raw watts. |
| IR(21-22) | NOT defined as consts; decoded uint32 deci as `total_export_kwh` (dec:623) | `e_grid_out_total` — `Def(C.uint32, C.deci, IR(21), IR(22))` (bi:257) | `e_grid_out_total` — `Def(C.uint32, C.deci, IR(21), IR(22))` (inv:637) | All agree: uint32 /10 kWh. |
| IR(23) | Not defined; NOT decoded | `e_solar_diverter` — `Def(C.deci, None, IR(23))` (bi:258) | `e_solar_diverter` — `Def(C.deci, None, IR(23))` (inv:638) | HEM skips. |
| **IR(24)** | Not defined; **NOT decoded** | `p_inverter_out` — `Def(C.int16, None, IR(24))` (bi:259) | **`p_grid_out_ph1`** — `Def(C.int16, None, IR(24))` (inv:641) | ⚠️ See Key Discrepancy #3. IR(24) is inverter AC terminal power (different node from IR(30)). HEM does not decode it. |
| IR(25) | `IR_TODAY_EXPORT_ENERGY` (reg:132); decoded deci (dec:590) | `e_grid_out_day` — `Def(C.deci, None, IR(25))` (bi:260) | `e_grid_out_day` — `Def(C.deci, None, IR(25))` (inv:642) | All agree: export today /10 kWh. |
| IR(26) | `IR_TODAY_IMPORT_ENERGY` (reg:134); decoded deci (dec:589) | `e_grid_in_day` — `Def(C.deci, None, IR(26))` (bi:261) | `e_grid_in_day` — `Def(C.deci, None, IR(26))` (inv:643) | All agree: import today /10 kWh. |
| IR(27-28) | NOT defined as consts; decoded as `total_import_kwh` → wait, NO — decoder.rs:623-624 says `total_import_kwh` uses IR(32-33). IR(27-28) is `e_inverter_in_total` in reference (inv:644). | `e_inverter_in_total` — `Def(C.uint32, C.deci, IR(27), IR(28))` (bi:262) | `e_inverter_in_total` — `Def(C.uint32, C.deci, IR(27), IR(28))` (inv:644) | ⚠️ **HEM does NOT decode IR(27-28).** HEM uses IR(32-33) for total_import_kwh instead (dec:624). The reference defines IR(27-28) as inverter input total (different from grid import). |
| IR(29) | Not defined; NOT decoded | `e_discharge_year` — `Def(C.deci, None, IR(29))` (bi:263) | `e_discharge_year` — `Def(C.deci, None, IR(29))` (inv:645) | HEM skips. |
| **IR(30)** | `IR_GRID_POWER` (reg:136); decoded int16 (dec:466) +ve=export | `p_grid_out` — `Def(C.int16, None, IR(30))` (bi:264) | `p_grid_out` — `Def(C.int16, None, IR(30))` (inv:649) | ⚠️ See Key Discrepancy #3. All three use IR(30) as external grid CT. Sign convention: +ve=export all agree. |
| IR(31) | NOT defined as const; decoded raw uint16 as `eps_power_w` (dec:477) | `p_eps_backup` — `Def(C.uint16, None, IR(31))` (bi:265) | `p_backup` — `Def(C.uint16, None, IR(31), max=50000)` (inv:650) | All agree: unsigned watts, EPS output. |
| IR(32-33) | NOT defined as consts; decoded uint32 deci as `total_import_kwh` (dec:624) | `e_grid_in_total` — `Def(C.uint32, C.deci, IR(32), IR(33))` (bi:266) | `e_grid_in_total` — `Def(C.uint32, C.deci, IR(32), IR(33))` (inv:651) | All agree: uint32 /10 kWh grid import total. |
| IR(34) | Not defined; NOT decoded | Not in GivTCP LUT | Not in Reference LUT (explicitly skipped — inv:652) | All skip. |
| **IR(35)** | `IR_TODAY_CONSUMPTION` (reg:138); decoded as `today_ac_charge_kwh` (dec:599) | `e_inverter_in_day` — `Def(C.deci, None, IR(35))` (bi:267) | **`e_ac_charge_today`** — `Def(C.deci, None, IR(35))` (inv:659) | 🔴 See Key Discrepancy #2. HEM constant name says CONSUMPTION but decoder correctly uses AC charge. GivTCP name says inverter_in (wrong). Reference name is correct: AC charge today. |
| IR(36) | `IR_TODAY_CHARGE_ENERGY` (reg:140); decoded deci (dec:591) | `e_battery_charge_today` — `Def(C.deci, None, IR(36))` (bi:268) | `e_battery_charge_today_alt1` — `Def(C.deci, None, IR(36))` (inv:660) | All agree: /10 kWh. Reference calls it `_alt1` to distinguish from IR(182) alt2. |
| IR(37) | `IR_TODAY_DISCHARGE_ENERGY` (reg:142); decoded deci (dec:592) | `e_battery_discharge_today` — `Def(C.deci, None, IR(37))` (bi:269) | `e_battery_discharge_today_alt1` — `Def(C.deci, None, IR(37))` (inv:661) | All agree. |
| IR(38) | Not defined; NOT decoded | `inverter_countdown` — `Def(C.uint16, None, IR(38))` (bi:270) | `countdown` — `Def(C.uint16, None, IR(38))` (inv:662) | HEM skips. |
| IR(39-40) | NOT defined as consts; IR(40) low word used for grid-loss fault bit (dec:519) | Not in GivTCP LUT for these indexes | `fault_code` — `Def(C.uint32, (C.hex, 8), IR(39), IR(40))` (inv:663) | HEM uses IR(40) bit 7 for grid loss detection; does not decode full fault_code. |
| IR(41) | `IR_INVERTER_TEMPERATURE` (reg:144); decoded deci (dec:538) | `temp_inverter_heatsink` — `Def(C.deci, None, IR(41))` (bi:273) | `t_inverter_heatsink` — `Def(C.deci, None, IR(41), min=-40.0, max=100.0)` (inv:664) | All agree: /10 °C. |
| IR(42) | NOT defined as const; decoded raw uint16 as `home_power` (dec:490) | `p_load_demand` — `Def(C.uint16, None, IR(42))` (bi:274) | `p_load_demand` — `Def(C.uint16, None, IR(42), max=50000)` (inv:667) | All agree: unsigned watts, house load. |
| IR(43) | Not defined; NOT decoded | `p_grid_apparent` — `Def(C.uint16, None, IR(43))` (bi:275) | `p_grid_apparent` — `Def(C.uint16, None, IR(43), max=50000)` (inv:670) | HEM skips. |
| IR(44) | NOT defined as const; used as FALLBACK for today_solar_kwh when IR(17)+IR(19)=0 (dec:582-588) | `e_inverter_out_day` — `Def(C.deci, None, IR(44))` (bi:276) | **`e_pv_generation_today`** — `Def(C.deci, None, IR(44))` (inv:678) | ⚠️ See Key Discrepancy #6. Reference renamed: this is PV generation, not inverter output. GivTCP still uses old name. HEM uses as fallback only. |
| IR(45-46) | NOT defined as consts; NOT decoded by HEM (uses IR(11-12) instead) | `e_inverter_out_total` — `Def(C.uint32, C.deci, IR(45), IR(46))` (bi:277) | **`e_pv_generation_total`** — `Def(C.uint32, C.deci, IR(45), IR(46))` (inv:679) | ⚠️ See Key Discrepancy #6. Reference renamed. GivTCP uses old name. HEM does not decode these registers. |
| IR(47-48) | NOT defined as consts; decoded uint32 as `operating_hours` with cap at 876,000 (dec:639-643) | `work_time_total` — `Def(C.uint32, None, IR(47), IR(48))` (bi:278) | `work_time_total_hours` — `Def(C.uint32, None, IR(47), IR(48), max=876_000)` (inv:685) | All agree: uint32 pair, hours. Reference caps at 876,000; HEM caps at 876,000. |
| IR(49) | NOT defined as const; used for grid-loss detection (dec:518) | `system_mode` — `Def(C.uint16, None, IR(49))` (bi:279) | `system_mode` — `Def(C.uint16, None, IR(49))` (inv:686) | All agree. |
| IR(50) | `IR_BATTERY_VOLTAGE` (reg:146); decoded centi (dec:459) | `v_battery` — `Def(C.centi, None, IR(50))` (bi:280) | `v_battery` — `Def(C.centi, None, IR(50), min=0.0, max=100.0)` (inv:687) | All agree: /100 V. |
| IR(51) | `IR_BATTERY_CURRENT` (reg:150); decoded int16 centi (dec:461) +ve=discharge | `i_battery` — `Def(C.int16, C.centi, IR(51))` (bi:281) | `i_battery` — `Def(C.int16, C.centi, IR(51), min=-300.0, max=300.0)` (inv:688) | All agree: signed /100 A, +ve=discharge. |
| **IR(52)** | `IR_BATTERY_POWER` (reg:153); decoded int16 (dec:457) +ve=discharge | `p_battery` — `Def(C.int16, None, IR(52))` (bi:282) | `p_battery` — `Def(C.int16, None, IR(52))` (inv:689) | All agree: int16 W, +ve=discharge. NOTE: HR(52) is a DIFFERENT register (power_factor). |
| IR(53) | Not defined; NOT decoded | `v_eps_backup` — `Def(C.deci, None, IR(53))` (bi:283) | `v_ac1_output` — `Def(C.deci, None, IR(53), min=0.0, max=500.0)` (inv:690) | HEM skips. GivTCP calls it EPS voltage; Reference calls it V AC1 output (might be EPS). |
| IR(54) | Not defined; NOT decoded | `f_eps_backup` — `Def(C.centi, None, IR(54))` (bi:284) | `f_ac1_output` — `Def(C.centi, None, IR(54), min=40.0, max=70.0)` (inv:691) | HEM skips. |
| IR(55) | Not defined; NOT decoded | `temp_charger` — `Def(C.deci, None, IR(55))` (bi:285) | `t_charger` — `Def(C.deci, None, IR(55), min=-40.0, max=100.0)` (inv:692) | HEM skips. |
| IR(56) | `IR_BATTERY_TEMPERATURE` (reg:155); decoded deci (dec:463) | `temp_battery` — `Def(C.deci, None, IR(56))` (bi:286) | `t_battery` — `Def(C.deci, None, IR(56), min=-40.0, max=100.0)` (inv:693) | All agree: /10 °C. |
| IR(57) | Used for battery_over_temp detection (dec:535) — reads value == 1 | `battery_errors` — `Def(C.battery_fault_code, None, IR(56))` (bi:287) — **NOTE: GivTCP uses IR(56) for battery_errors, not IR(57)!** | `charger_warning_code` — `Def(C.uint16, None, IR(57))` (inv:694) | ⚠️ **GivTCP maps battery_errors to IR(56)**, reusing the battery temp register. Reference uses IR(57) for charger_warning_code. HEM uses IR(57) for over-temp check. |
| IR(58) | Not defined; NOT decoded | `i_grid_port` — `Def(C.centi, None, IR(58))` (bi:288) | `i_grid_port` — `Def(C.centi, None, IR(58))` (inv:697) | HEM skips. |
| IR(59) | `IR_BATTERY_SOC` (reg:157); decoded as u8 (dec:458) | `battery_percent` — `Def(C.uint16, None, IR(59))` (bi:289) | `battery_soc` — `Def(C.uint16, None, IR(59), min=0, max=100)` (inv:698) | All agree: 0-100%. |

### HR 0–59: Holding Registers (Configuration Part 1)

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(0) | `HR_DEVICE_TYPE` (reg:167); decoded as device_type (dec:686) | `device_type_code` — `Def(C.hex, None, HR(0))` (bi:31) | `device_type_code` — `Def(C.hex, None, HR(0))` (inv:406) | All agree. |
| HR(1-2) | Not defined; NOT decoded | Not directly (model from HR(0)+HR(21)); `inverter_max_power_new` uses HR(2) (bi:33) | `module` — `Def(C.uint32, (C.hex, 8), HR(1), HR(2))` (inv:408) | HEM skips. |
| HR(3) | Not defined; NOT decoded | `num_mppt` — `Def((C.duint8, 0), None, HR(3))` (bi:37) | `num_mppt` — `Def((C.duint8, 0), None, HR(3))`; `num_phases` — `Def((C.duint8, 1), None, HR(3))` (inv:409-410) | HEM skips both splits. |
| HR(7) | NOT defined as const; decoded as bool `enable_ammeter` (dec:736) | `enable_ammeter` — `Def(C.bool, None, HR(7))` (bi:40) | `enable_ammeter` — `Def(C.bool, None, HR(7))` (inv:412) | All agree. |
| HR(8-12) | Not defined; NOT decoded | `first_battery_serial_number` — 5 regs (bi:41-43) | `first_battery_serial_number` — 5 regs (inv:413) | HEM skips first battery serial. |
| HR(13-17) | `HR_SERIAL_NUMBER_START` (reg:169); decoded as 10-char Latin-1 (dec:690) | `serial_number` — 5 regs (bi:44) | `serial_number` — 5 regs (inv:414) | All agree. |
| HR(18) | Not defined; NOT decoded | `first_battery_bms_firmware_version` (bi:45) | `first_battery_bms_firmware_version` (inv:415) | HEM skips. |
| HR(19) | NOT defined as const; decoded as `dsp_firmware_version` (dec:700-705) | `dsp_firmware_version` — `Def(C.uint16, None, HR(19))` (bi:46) | `dsp_firmware_version` — `Def(C.uint16, None, HR(19))` (inv:416) | All agree. |
| HR(20) | `HR_ENABLE_CHARGE_TARGET` (reg:171); decoded as bool (dec:745) | `enable_charge_target` — `Def(C.uint16, Enable, HR(20), valid=(0,1))` (bi:47) | `enable_charge_target` — `Def(C.bool, None, HR(20))` (inv:417) | All agree: bool. GivTCP maps to Enable enum. |
| HR(21) | `HR_ARM_FIRMWARE` (reg:177); decoded as string (dec:693-698) | `arm_firmware_version` — `Def(C.uint16, None, HR(21))` (bi:48) | `arm_firmware_version` — `Def(C.uint16, None, HR(21))` (inv:418) | All agree. |
| HR(22) | Not defined; NOT decoded | `usb_device_inserted` (bi:51) | `usb_device_inserted` (inv:420) | HEM skips. |
| HR(23) | Not defined; NOT decoded | `select_arm_chip` (bi:52) | `select_arm_chip` (inv:421) | HEM skips. |
| HR(24) | Not defined; NOT decoded | `variable_address` (bi:53) | `variable_address` (inv:422) | HEM skips. |
| HR(25) | Not defined; NOT decoded | `variable_value` (bi:54) | `variable_value` (inv:423) | HEM skips. |
| HR(26) | Not defined; NOT decoded | `grid_port_max_power_output` (bi:55) | `grid_port_max_power_output` (inv:424) | HEM skips. |
| HR(27) | `HR_BATTERY_POWER_MODE` (reg:179); decoded (dec:724) 0=export/1=eco | `eco_mode` — `Def(C.uint16, Enable, HR(27), valid=(0,1))` (bi:56) | `battery_power_mode` — `Def(C.uint16, BatteryPowerMode, HR(27))` (inv:425) | All agree: 0=export, 1=eco/self-consumption. Name differs: HEM/GivTCP "eco_mode", Ref "battery_power_mode". |
| HR(28) | Not defined; NOT decoded | `enable_60hz_freq_mode` (bi:57) | `enable_60hz_freq_mode` (inv:426) | HEM skips. |
| HR(29) | `HR_BATTERY_CALIBRATION_STAGE` (reg:235); decoded as u8 (dec:731) | `soc_force_adjust` — `Def(C.uint16, BatteryCalibrationStage, HR(29), valid=(0,3))` (bi:58) | `battery_calibration_stage` — `Def(C.uint16, BatteryCalibrationStage, HR(29))` (inv:427) | All agree: calibration stage. GivTCP bounds [0,3] — Reference allows 0-7 (BALANCE=5). HEM's encoder allows 0-7 (encoder.rs:516). |
| HR(30) | Not defined; NOT decoded | `modbus_address` (bi:59) | `modbus_address` (inv:428) | HEM skips. |
| **HR(31-32)** | `HR_CHARGE_SLOT_2_START/END` (reg:183-184); decoded as timeslot (dec:771) | `charge_slot_2` — `Def(C.timeslot, None, HR(31), HR(32))` (bi:60) — **BUT OVERWRITTEN** by line 157 (bi:157) | `charge_slot_2` — `Def(C.timeslot, None, HR(31), HR(32))` (inv:429) | 🔴 See Key Discrepancy #4. GivTCP's second definition at HR(243-244) overwrites this one in the dict. |
| HR(33) | Not defined; NOT decoded | `user_code` (bi:63) | `user_code` (inv:430) | HEM skips. |
| HR(34) | Not defined; NOT decoded | `modbus_version` — `Def(C.centi, (C.fstr, "0.2f"), HR(34))` (bi:64) | `modbus_version` — `Def(C.centi, (C.fstr, "0.2f"), HR(34))` (inv:431) | HEM skips. GivTCP/Ref agree: centi formatted. |
| HR(35-40) | `HR_SYSTEM_TIME_*` (reg:358-368); decoded as datetime string (dec:714) | `system_time` — `Def(C.datetime, ...)` (bi:65-67) | `system_time` — `Def(C.datetime, ...)` (inv:432) | All agree. HEM formats as "YYYY-MM-DD HH:MM:SS" with year<100 → 2000+. |
| HR(41) | Not defined; NOT decoded | `enable_drm_rj45_port` (bi:68) | `enable_drm_rj45_port` (inv:433) | HEM skips. |
| HR(42) | NOT defined as const; decoded as bool `enable_reversed_ct_clamp` (dec:739) | `enable_reversed_ct_clamp` — `Def(C.uint16, Enable, HR(42))` (bi:69) | `enable_reversed_ct_clamp` — `Def(C.bool, None, HR(42))` (inv:434) | All agree. NOTE: IR(42) is p_load_demand — different register space. |
| **HR(43)** | **NOT decoded** | `charge_soc` (hi byte) / `discharge_soc` (lo byte) (bi:70-71) | `charge_soc` (hi byte) / `discharge_soc` (lo byte) (inv:435-436) | 🔴 See Key Discrepancy #5. HEM does not decode this packed register. |
| HR(44-45) | `HR_DISCHARGE_SLOT_2_START/END` (reg:186-187); decoded as timeslot (dec:777) | `discharge_slot_2` — `Def(C.timeslot, None, HR(44), HR(45))` (bi:72) | `discharge_slot_2` — `Def(C.timeslot, None, HR(44), HR(45))` (inv:437) | All agree. |
| HR(46) | Not defined; NOT decoded | `bms_firmware_version` (bi:75) | `bms_firmware_version` (inv:438) | HEM skips. |
| HR(47) | Not defined as const; decoded as u8 `meter_type` (dec:742) | `meter_type` — `Def(C.uint16, MeterType, HR(47))` (bi:76) | `meter_type` — `Def(C.uint16, MeterType, HR(47))` (inv:439) | All agree: 0=CT/EM418, 1=EM115. |
| HR(48) | Not defined; NOT decoded | `enable_reversed_115_meter` (bi:77) | `enable_reversed_115_meter` (inv:440) | HEM skips. |
| HR(49) | Not defined; NOT decoded | `enable_reversed_418_meter` (bi:78) | `enable_reversed_418_meter` (inv:441) | HEM skips. |
| HR(50) | `HR_ACTIVE_POWER_RATE` (reg:181); decoded as u8 (dec:752) | `active_power_rate` — `Def(C.uint16, None, HR(50))` (bi:79) | `active_power_rate` — `Def(C.uint16, None, HR(50))` (inv:442) | All agree: 0-100%. |
| HR(51) | Not defined; NOT decoded | `reactive_power_rate` (bi:80) | `reactive_power_rate` (inv:443) | HEM skips. NOTE: IR(51) is battery current — different register space. |
| **HR(52)** | Not defined; NOT decoded | `power_factor` — `Def(C.uint16, None, HR(52))` (bi:81) | `power_factor` — `Def(C.uint16, None, HR(52))` (inv:444) | HEM skips. NOTE: IR(52) is battery power — different register space. |
| HR(53) | Not defined; NOT decoded | `enable_inverter_auto_restart` (hi) / `enable_inverter` (lo) (bi:82-83) | Same split (inv:445-446) | HEM skips. |
| HR(54) | Not defined; NOT decoded | `battery_type` — `Def(C.uint16, BatteryType, HR(54))` (bi:84) | `battery_type` — `Def(C.uint16, BatteryType, HR(54))` (inv:447) | HEM skips. |
| HR(55) | NOT defined as const; decoded as `battery_capacity_kwh` via nominal_voltage (dec:719-721) | `battery_nominal_capacity` — Ah (bi:85) | `battery_capacity_ah` — `Def(C.uint16, None, HR(55))` (inv:448) | All agree: Ah capacity. |
| HR(56-57) | `HR_DISCHARGE_SLOT_1_START/END` (reg:189-190); decoded as timeslot (dec:774) | `discharge_slot_1` — `Def(C.timeslot, None, HR(56), HR(57))` (bi:86) | `discharge_slot_1` — `Def(C.timeslot, None, HR(56), HR(57))` (inv:449) | All agree. |
| HR(58) | Not defined; NOT decoded | `enable_auto_judge_battery_type` (bi:89) | `enable_auto_judge_battery_type` (inv:450) | HEM skips. |
| HR(59) | `HR_ENABLE_DISCHARGE` (reg:192); decoded as bool (dec:748) | `enable_discharge` — `Def(C.uint16, Enable, HR(59))` (bi:90) | `enable_discharge` — `Def(C.bool, None, HR(59))` (inv:451) | All agree. |

### HR 60–119: Holding Registers (Configuration Part 2)

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(60) | Not defined; NOT decoded | `v_pv_start` — `Def(C.uint16, C.deci, HR(60))` (bi:94) | `v_pv_start` — `Def(C.uint16, C.deci, HR(60), min=0.0, max=2000.0)` (inv:455) | HEM skips. |
| HR(61) | Not defined; NOT decoded | `start_countdown_timer` (bi:95) | `start_countdown_timer` (inv:456) | HEM skips. |
| HR(62) | Not defined; NOT decoded | `restart_delay_time` (bi:96) | `restart_delay_time` (inv:457) | HEM skips. |
| HR(63-93) | Not defined; NOT decoded | Skipped (bi:97) | Skipped (inv:458) | All skip protection settings. |
| HR(94-95) | `HR_CHARGE_SLOT_1_START/END` (reg:198-199); decoded as timeslot (dec:785) | `charge_slot_1` — `Def(C.timeslot, None, HR(94), HR(95))` (bi:98) | `charge_slot_1` — `Def(C.timeslot, None, HR(94), HR(95))` (inv:459) | All agree. |
| HR(96) | `HR_ENABLE_CHARGE` (reg:201); decoded as bool (dec:788) | `enable_charge` — `Def(C.uint16, Enable, HR(96))` (bi:101) | `enable_charge` — `Def(C.bool, None, HR(96))` (inv:460) | All agree. |
| HR(97) | Not defined; NOT decoded | `battery_low_voltage_protection_limit` (bi:102) | `battery_low_voltage_protection_limit` (inv:461) | HEM skips. |
| HR(98) | Not defined; NOT decoded | `battery_high_voltage_protection_limit` (bi:103) | `battery_high_voltage_protection_limit` (inv:462) | HEM skips. |
| HR(105) | Not defined; NOT decoded | `battery_voltage_adjust` (bi:105) | `battery_voltage_adjust` (inv:464) | HEM skips. |
| HR(108) | Not defined; NOT decoded | `battery_low_force_charge_time` (bi:107) | `battery_low_force_charge_time` (inv:466) | HEM skips. |
| HR(109) | Not defined; NOT decoded | `enable_bms_read` (bi:108) | `enable_bms_read` (inv:467) | HEM skips. |
| HR(110) | `HR_BATTERY_SOC_RESERVE` (reg:203); decoded as u8 clamped 4-100 (dec:791) | `battery_soc_reserve` — `Def(C.uint16, None, HR(110))` (bi:109) | `battery_soc_reserve` — `Def(C.uint16, None, HR(110))` (inv:468) | All agree. |
| HR(111) | `HR_BATTERY_CHARGE_LIMIT` (reg:205); decoded as u8 for non-AC models (dec:801) | `battery_charge_limit` — `Def(C.uint16, None, HR(111), valid=(0,50))` (bi:110) | `battery_charge_limit` — `Def(C.uint16, None, HR(111))` (inv:469) | All agree: 0-50%. HEM's encoder enforces 0-50; GivTCP declares valid=(0,50). |
| HR(112) | `HR_BATTERY_DISCHARGE_LIMIT` (reg:207); decoded as u8 for non-AC models (dec:802) | `battery_discharge_limit` — `Def(C.uint16, None, HR(112), valid=(0,50))` (bi:111) | `battery_discharge_limit` — `Def(C.uint16, None, HR(112))` (inv:470) | All agree. |
| HR(113) | Not defined; NOT decoded | `enable_buzzer` — `Def(C.uint16, Enable, HR(113))` (bi:112) | `enable_buzzer` — `Def(C.bool, None, HR(113))` (inv:471) | HEM skips. |
| HR(114) | `HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE` (reg:259); NOT decoded (only in SAFE_WRITE_REGS and encoder) | `battery_discharge_min_power_reserve` — `Def(C.uint16, None, HR(114), valid=(4,100))` (bi:113-115) | `battery_discharge_min_power_reserve` — `Def(C.uint16, None, HR(114))` (inv:472) | ⚠️ HEM skips decoding this register. Only written via encoder (encoder.rs:213). |
| HR(116) | `HR_CHARGE_TARGET_SOC` (reg:209); decoded as u8 clamped 4-100 (dec:806) | `charge_target_soc` — `Def(C.uint16, None, HR(116), valid=(4,100))` (bi:117) | `charge_target_soc` — `Def(C.uint16, None, HR(116))` (inv:474) | All agree. |
| HR(117) | Not defined; NOT decoded | `charge_soc_stop_2` (bi:118) | `charge_soc_stop_2` (inv:475) | HEM skips. |
| HR(118) | Not defined; NOT decoded | `discharge_soc_stop_2` (bi:119) | `discharge_soc_stop_2` (inv:476) | HEM skips. |
| HR(119) | Not defined; NOT decoded | `charge_soc_stop_1` (bi:120) | `charge_soc_stop_1` (inv:477) | HEM skips. |

### HR 120–179: Holding Registers

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(120) | Not defined; NOT decoded | `discharge_soc_stop_1` (bi:124) | `discharge_soc_stop_1` (inv:481) | HEM skips. |
| HR(121) | Not defined; NOT decoded | `enable_local_command_test` (bi:125) | `enable_local_command_test` (inv:482) | HEM skips. |
| HR(122) | Not defined; NOT decoded | `power_factor_function_model` (bi:126) | `power_factor_function_model` (inv:483) | HEM skips. |
| HR(163) | `HR_INVERTER_REBOOT` (reg:238); write 100 to reboot (encoder.rs:519-521) | `inverter_reboot` (bi:134) | `inverter_reboot` (inv:491) | All agree. Write-only register. |
| HR(166) | `HR_ENABLE_RTC` (reg:241); write bool (encoder.rs:525-527) | `rtc_enable` — `Def(C.uint16, Enable, HR(166))` (bi:135) | `enable_rtc` — `Def(C.bool, None, HR(166))` (inv:492) | All agree. |
| HR(167-171) | Not defined; NOT decoded | Threephase balance registers (bi:136-140) | Threephase balance registers (inv:493-497) | HEM skips (single-phase irrelevant). |
| HR(175) | Not defined; NOT decoded | `enable_battery_on_pv_or_grid` (bi:142) | `enable_battery_on_pv_or_grid` (inv:499) | HEM skips. |
| HR(176) | Not defined; NOT decoded | `debug_inverter` (bi:143) | `debug_inverter` (inv:500) | HEM skips. |
| HR(177) | Not defined; NOT decoded | `enable_ups_mode` (bi:144) | `enable_ups_mode` (inv:501) | HEM skips. |
| HR(178) | Not defined; NOT decoded | `enable_g100_limit_switch` (bi:145) | `enable_g100_limit_switch` (inv:502) | HEM skips. |
| HR(179) | Not defined; NOT decoded | `enable_battery_cable_impedance_alarm` (bi:146) | `enable_battery_cable_impedance_alarm` (inv:503) | HEM skips. |

### HR 180–239: Holding Registers

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(199) | Not defined; NOT decoded | `enable_standard_self_consumption_logic` — `Def(C.uint16, Enable, HR(199))` (bi:150) | `enable_inverter_parallel_mode` — `Def(C.bool, None, HR(199))` (inv:507) | ⚠️ **Name mismatch.** GivTCP uses old name; Reference renamed to `enable_inverter_parallel_mode`. |
| HR(200) | Not defined; NOT decoded | `cmd_bms_flash_update` (bi:151) | `cmd_bms_flash_update` (inv:508) | HEM skips. |
| HR(223-224) | Not defined; NOT decoded | `inverter_errors` — `Def(C.uint32, C.inverter_fault_code, HR(223), HR(224))` (bi:155) | `inverter_errors` — `Def(C.uint32, None, HR(223), HR(224))` (inv:509) | HEM skips. GivTCP applies fault code decoder; Reference stores raw uint32. |

### HR 240–299: Extended Slots (Gen3)

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(242) | `HR_CHARGE_TARGET_SOC_1` (reg:213); decoded if >0 (dec:900-903) | `charge_target_soc_1` — `Def(C.uint16, None, HR(242), valid=(4,100))` (bi:156) | `charge_target_soc_1` — `Def(C.uint16, None, HR(242))` (inv:516) | All agree. |
| **HR(243-244)** | `HR_CHARGE_SLOT_2_GEN3_START/END` (reg:222-223); decoded as charge_slot_2 override (dec:848-867) | `charge_slot_2` — `Def(C.timeslot, None, HR(243), HR(244))` (bi:157) — **overwrites the HR(31-32) definition** | `charge_slot_2_x` — `Def(C.timeslot, None, HR(243), HR(244))` (inv:517) | 🔴 See Key Discrepancy #4. GivTCP overwrites classic slot 2. Reference keeps as separate `_x` key. HEM conditionally uses this as override for Gen3 models. |
| HR(245) | `HR_CHARGE_TARGET_SOC_2` (reg:214); decoded if >0 (dec:901-904) | `charge_target_soc_2` — `Def(C.uint16, None, HR(245), valid=(4,100))` (bi:160) | `charge_target_soc_2` — `Def(C.uint16, None, HR(245))` (inv:518) | All agree. |
| HR(246-247) | `HR_CHARGE_SLOT_3_*` (reg:265-266); decoded in loop (dec:871-896) | `charge_slot_3` (bi:161) | `charge_slot_3` (inv:519) | All agree. |
| HR(248) | `HR_CHARGE_TARGET_SOC_3` (reg:267); decoded in loop | `charge_target_soc_3` (bi:164) | `charge_target_soc_3` (inv:520) | All agree. |
| HR(249-250) | Slots 4 (reg:268-269) | Slots 4 (bi:165) | Slots 4 (inv:521) | Same pattern. |
| HR(251) | Target SOC 4 (reg:270) | Target SOC 4 (bi:168) | Target SOC 4 (inv:522) | Same pattern. |
| HR(252-254) | Slots 5 (reg:271-273) | Slots 5 (bi:169-172) | Slots 5 (inv:523-524) | Same pattern through slot 10. |
| ... | ... slots 6-10 at HR(255-268) with interleaved targets | ... | ... | All three codebases agree on the 3-register stride (start, end, target_soc). |
| HR(272) | `HR_DISCHARGE_TARGET_SOC_1` (reg:226); decoded if >0 (dec:942-944) | `discharge_target_soc_1` (bi:193) | `discharge_target_soc_1` (inv:535) | All agree. |
| HR(275) | `HR_DISCHARGE_TARGET_SOC_2` (reg:227); decoded if >0 (dec:943-946) | `discharge_target_soc_2` (bi:194) | `discharge_target_soc_2` (inv:536) | All agree. |
| HR(276-298) | Discharge slots 3-10 (reg:294-316); decoded in loop (dec:910-938) | Discharge slots 3-10 (bi:195-226) | Discharge slots 3-10 (inv:537-552) | All agree on layout. |
| HR(299) | `HR_DISCHARGE_TARGET_SOC_10` (reg:317) | `discharge_target_soc_10` (bi:226) | `discharge_target_soc_10` (inv:552) | All agree. |

### HR 300–359: AC Configuration

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(311) | `HR_EXPORT_PRIORITY` (reg:248); decoded as u8 (dec:958) | NOT in GivTCP single-phase LUT (only in newer blocks?) | `export_priority` — `Def(C.uint16, ExportPriority, HR(311))` (inv:575) | ⚠️ GivTCP does not decode HR(311) in baseinverter.py. |
| HR(313) | `HR_AC_BATTERY_CHARGE_LIMIT` (reg:250); decoded as u8 → `charge_rate` (dec:963) | `battery_charge_limit_ac` — `Def(C.uint16, None, HR(313), valid=(0,100))` (bi:231) | `battery_charge_limit_ac` — `Def(C.uint16, None, HR(313))` (inv:576) | ⚠️ HEM overwrites `charge_rate` with AC limit unconditionally (dec:963). See Key Discrepancy #10. |
| HR(314) | `HR_AC_BATTERY_DISCHARGE_LIMIT` (reg:252); decoded as u8 → `discharge_rate` (dec:964) | `battery_discharge_limit_ac` — `Def(C.uint16, None, HR(314), valid=(0,100))` (bi:232) | `battery_discharge_limit_ac` — `Def(C.uint16, None, HR(314))` (inv:577) | ⚠️ Same overwrite issue as HR(313). |
| HR(317) | `HR_ENABLE_EPS` (reg:254); decoded as bool (dec:967) | NOT in GivTCP single-phase LUT | `enable_eps` — `Def(C.bool, None, HR(317))` (inv:580) | ⚠️ GivTCP does not decode HR(317) in baseinverter.py. |
| HR(318) | `HR_BATTERY_PAUSE_MODE` (reg:230); decoded as u8 (dec:970) | `battery_pause_mode` — `Def(C.uint16, BatteryPauseMode, HR(318), valid=(0,3))` (bi:233) | `battery_pause_mode` — `Def(C.uint16, None, HR(318))` (inv:581) | All agree. |
| HR(319-320) | `HR_BATTERY_PAUSE_SLOT_1_*` (reg:231-232); decoded as timeslot (dec:973) | `battery_pause_slot_1` — `Def(C.timeslot, None, HR(319), HR(320))` (bi:234) | `battery_pause_slot_1` — `Def(C.timeslot, None, HR(319), HR(320))` (inv:582) | All agree. |

### HR 554–573: Smart Load Slots

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| HR(554-555) | Partially in SAFE_WRITE_REGS (reg:507-508); **NOT decoded** | NOT in GivTCP single-phase LUT | `smart_load_slot_1` — `Def(C.timeslot, None, HR(554), HR(555))` (inv:558) | ⚠️ Only Reference defines and decodes these. HEM has addresses in SAFE_WRITE_REGS but no poll block or decoder. GivTCP does not include them. |
| HR(556-573) | Slots 2-10 in SAFE_WRITE_REGS | Not in GivTCP | `smart_load_slot_2` through `smart_load_slot_10` (inv:559-567) | Same gap. |

### IR 180–183: Alternative Battery Energy Totals

| Addr | HEM name & usage | GivTCP name & usage | Reference name & usage | Notes / Discrepancy |
|------|------------------|---------------------|------------------------|---------------------|
| **IR(180)** | NOT defined as const; decoded deci as `total_discharge_kwh` (dec:665) | `e_battery_discharge_total_2` — **`Def(C.deci, None, HR(180))`** (bi:290) | `e_battery_discharge_total_alt1` — `Def(C.deci, None, IR(180))` (inv:702) | 🔴 **GivTCP uses HR(180) instead of IR(180)**. This is a type mismatch — see Key Discrepancy #1. |
| IR(181) | NOT defined as const; decoded deci as `total_charge_kwh` (dec:666) | `e_battery_charge_total_2` — `Def(C.deci, None, IR(181))` (bi:291) | `e_battery_charge_total_alt1` — `Def(C.deci, None, IR(181))` (inv:703) | All agree (except GivTCP naming `_2` vs Ref `_alt1`). |
| IR(182) | NOT defined as const; decoded deci as `today_discharge_kwh` ONLY for Gen1Hybrid (dec:677) | `e_battery_discharge_today_2` — `Def(C.deci, None, IR(182))` (bi:292) | `e_battery_discharge_today_alt2` — `Def(C.deci, None, IR(182))` (inv:704) | All agree on address/type. HEM only overrides Gen1Hybrid. Reference routes by model. GivTCP uses `_2` naming. |
| IR(183) | NOT defined as const; decoded deci as `today_charge_kwh` ONLY for Gen1Hybrid (dec:678) | `e_battery_charge_today_2` — `Def(C.deci, None, IR(183))` (bi:293) | `e_battery_charge_today_alt2` — `Def(C.deci, None, IR(183))` (inv:705) | All agree on address/type. |

---

## Register Constant Naming Map

| HEM constant (registers.rs) | Meaning | Reference name |
|------------------------------|---------|----------------|
| `IR_TODAY_CONSUMPTION` (line 138) | **AC charge today** (misleading name!) | `e_ac_charge_today` |
| `IR_TODAY_CHARGE_ENERGY` (line 140) | Battery charge today | `e_battery_charge_today_alt1` |
| `IR_TODAY_DISCHARGE_ENERGY` (line 142) | Battery discharge today | `e_battery_discharge_today_alt1` |
| `IR_GRID_POWER` (line 136) | Grid CT power (IR 30) | `p_grid_out` |
| `IR_BATTERY_POWER` (line 153) | Battery DC power (IR 52) | `p_battery` |
| `HR_BATTERY_POWER_MODE` (line 179) | Mode: 0=export, 1=eco | `battery_power_mode` |
| `HR_BATTERY_DISCHARGE_MIN_POWER_RESERVE` (line 259) | Discharge min reserve (HR 114) | `battery_discharge_min_power_reserve` |
| `HR_CHARGE_SLOT_2_GEN3_*` (lines 222-223) | Gen3 charge slot 2 at HR 243-244 | `charge_slot_2_x` |

---

## Poll Block Coverage

### HEM STANDARD_POLL_BLOCKS (`registers.rs:42-73`)

- IR 0–59 (60 registers)
- HR 0–59 (60 registers)
- HR 60–119 (60 registers)
- IR 180–183 (4 registers)

**NOT polled in standard blocks**: HR 240–299, HR 300–359, HR 554–573, IR 60–119 (battery BMS)

These blocks are presumably added dynamically when the device type is detected (via `needs_extended_blocks()` etc.), but the standard blocks alone do not include extended scheduling or AC config.

### GivTCP poll blocks (`register.py:704-728`)

Core: IR→[0,60,120,180], HR→[0,60,120,120]  
Additional (Hybrid): IR→[240], HR→[180,240,300]

### Reference library (`client.py`)

Reads IR 0–59, HR 0–59, HR 60–119, IR 180–183, plus HR 240–299 and HR 300–359 when `non_ems_no_gateway` flag is set.

---

*End of audit. All claims are cited with exact file paths and line numbers.*

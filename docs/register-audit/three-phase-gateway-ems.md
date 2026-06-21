# Three-Phase / Gateway / EMS Register Audit

Register-by-register comparison across three codebases:

- **HEM** (givenergy-local): `src-tauri/src/modbus/registers.rs` + `src-tauri/src/inverter/decoder.rs`
- **GivTCP**: `GivTCP/givenergy_modbus_async/model/threephase.py` + `gateway.py` + `ems.py`
- **Reference** (givenergy-modbus): `givenergy_modbus/model/inverter_threephase.py` + `gateway.py` + `ems.py`

> **Note on register spaces**: IR and HR are separate Modbus address spaces. Three-phase inverters shadow many single-phase registers at higher addresses (HR 1080-1124, IR 1000-1413). Gateway uses IR 1600-1859. EMS uses HR 2040-2075 (holding) and IR 2040-2094 (input).

---

## KEY DISCREPANCIES SUMMARY

Each discrepancy is numbered with file:line evidence from all three codebases.

### D1. HEM decode_holding_1000_1079 IS A NO-OP — HR 1005 and 1078 never decoded

**HEM**: `decoder.rs:995-999` — `decode_holding_1000_1079` takes `_data, _snap, _raw` but performs NO decoding. Comments acknowledge "HR 1005 and 1078 are in SAFE_WRITE_REGS but not yet displayed on the dashboard."

**Reference**: `inverter_threephase.py:202` — `"set_command_save": Def(C.bool, None, HR(1001))`, and grid-connection parameters HR 1002-1079 are fully defined. HR(1005) itself is NOT in the reference LUT — it's documented in code comments as REAL_TIME_CONTROL (three-phase mirror of HR 166 ENABLE_RTC).

**GivTCP**: `threephase.py:35-105` — Has `"set_command_save": Def(C.bool, None, HR(1001))` and full grid-parameter definitions but also has NO entry at HR(1005). Does have `"set_command_save"` at HR(1001) and `"battery_power_cutoff": Def(C.uint16, None, HR(1078))`.

**Impact**: HEM cannot display or restore the three-phase real-time control setting (HR 1005). The block is read (to prevent "unknown block" warnings) but the data is discarded. HR 1078 (battery_power_cutoff/battery_reserve_percent) is also not decoded.

### D2. HEM SAFE_WRITE_REGS groups HR(2040) with discharge slots — register semantics mismatch

**HEM**: `registers.rs:499-500` — Comment says "EMS plant-level control / discharge slots" and lists `2040, 2044, 2045, ... 2052`.

**Reference**: `ems.py:59` — HR(2040) is `"plant_status": Def(C.uint16, Status, HR(2040))` — the master plant enable/disable toggle, NOT a discharge slot start/end. Discharge slots start at HR(2044,2045).

**GivTCP**: `ems.py:35` — HR(2040) is `"plant_status": Def(C.uint16, Status, HR(2040))` — same as reference.

**Impact**: Functionally correct (HR 2040 IS writable and controls plant enable), but the comment is misleading. If any EMS write pipeline routes HR 2040 through a slot-time encoder, it would corrupt the plant_status register.

### D3. GivTCP gateway does NOT handle V2 byte order for energy totals

**Reference**: `gateway.py:141-156` — V2 firmware (GA000010+) swaps register order for ALL uint32 energy totals: `IR(1642),IR(1641)` vs V1 `IR(1641),IR(1642)`. Also shifts AIO serial addresses: V2 serials at 1841+ vs V1 at 1831+.

**HEM**: `decoder.rs:1619-1627` — `gw_u32()` function handles both V1 (hi,lo) and V2 (lo,hi) byte orders. V2 detection from IR(1603) >= 10. AIO serials use V1/V2-aware decode in `decode_gateway_1831_1859:1861-1877`.

**GivTCP**: `gateway.py:53-57` — Always uses V1 byte order for energy totals: `"e_grid_import_total": Def(C.uint32, C.deci, IR(1641),IR(1642))`. `read.py:1683` checks `if swv>9` only for serial numbers (`aio*_serial_number_new`), but energy totals are NEVER swapped.

**Impact**: Gateway V2 firmware owners using GivTCP get garbage energy total values (bytes swapped within the uint32). HEM handles this correctly.

### D4. GivTCP has duplicate battery_type at HR(1012) nibble — conflicts with reference

**GivTCP**: `threephase.py:45` — `"battery_type": Def((C.hexfield,1), BatteryType, HR(1012))` — extracts nibble 1 from HR(1012). Then `threephase.py:106` — `"battery_type": Def(C.uint16, BatteryType, HR(1080))` — RE-DEFINES battery_type at HR(1080), overwriting the hexfield definition.

**Reference**: `inverter_threephase.py:206` — HR(1012) is explicitly NOT decoded: "battery_type is at HR(1080)". `inverter_threephase.py:272` — `"battery_type": Def(C.uint16, BatteryType, HR(1080))`.

**HEM**: `registers.rs:323-348` — HR(1080-1124) block defined but NO battery_type constant is defined for three-phase. HEM never decodes battery_type from three-phase HRs.

**Impact**: GivTCP's battery_type always comes from HR(1080) (the second definition wins via dict update). The HR(1012) hexfield definition is dead code. HEM does not decode battery_type at all for three-phase models.

### D5. HEM does not decode HR 1124 (battery_maintenance_mode)

**Reference**: `inverter_threephase.py:309` — `"battery_maintenance_mode": Def(C.uint16, BatteryMaintenance, HR(1124))`

**GivTCP**: `threephase.py:139` — `"battery_maintenance_mode": Def(C.uint16, BatteryMaintenance, HR(1124))`

**HEM**: No `HR_3PH_BATTERY_MAINTENANCE` constant in `registers.rs`. Not in `SAFE_WRITE_REGS`. Not decoded in `decode_holding_1080_1124` (decoder.rs:1001-1020). The THREE_PHASE_CONFIG_BLOCK reads count=45 (covers HR 1080-1124, 45 registers) so HR 1124 IS read but discarded.

**Impact**: HEM cannot read or control three-phase battery maintenance mode.

### D6. HEM three-phase battery_type at IR(1121) not decoded

**Reference**: `inverter_threephase.py:362` — `"battery_type_ir": Def(C.uint16, BatteryType, IR(1121))`

**GivTCP**: `threephase.py:185` — `"battery_type": Def(C.int16, BatteryType, IR(1121))` (note: int16 vs uint16 in reference)

**HEM**: Not decoded from IR(1121). The `decode_input_1120_1179` function (decoder.rs:1136-1154) skips IR(1121) entirely.

**Impact**: Minor — the read-only battery type at IR(1121) is informational only.

### D7. GivTCP: three-phase system_mode has wrong converter

**GivTCP**: `threephase.py:164` — `"system_mode": Def(C.bool, SystemMode, IR(1075))` — uses C.bool converter!

**Reference**: `inverter_threephase.py:341` — `"system_mode": Def(C.uint16, None, IR(1075))` — uses C.uint16, no enum.

**HEM**: Does not decode IR(1075) explicitly but the three-phase block 2 is read.

**Impact**: GivTCP's system_mode for three-phase will be truncated to 0/1 (boolean) instead of the full WorkMode enum (0-4). This was likely a copy-paste error.

### D8. GivTCP EMS: IR(35) labeled e_inverter_in_day but reference says e_ac_charge_today

**GivTCP**: `ems.py:93` — `"e_inverter_in_day": Def(C.deci, None, IR(35))`

**Reference**: `inverter.py` — IR(35) renamed from `e_load_day` to `e_ac_charge_today` (AC charge from grid → battery, NOT house consumption). This was confirmed via sentinel cross-correlation (#174).

**HEM**: `decoder.rs:599` — IR(35) is `today_ac_charge_kwh` (correct).

**Impact**: GivTCP EMS module mislabels IR(35), which could confuse debugging. The value is the same, just misnamed.

### D9. GivTCP inverter_threephase.py: HR(1078) named battery_power_cutoff vs battery_reserve_percent

**GivTCP**: `threephase.py:104` — `"battery_power_cutoff": Def(C.uint16, None, HR(1078))`

**Reference**: `inverter_threephase.py:269` — `"battery_power_cutoff": Def(C.uint16, None, HR(1078))` (same name)

**HEM**: `registers.rs:497-498` — Comment calls it `BATTERY_RESERVE_PERCENT (three-phase)` but the register content is actually battery_power_cutoff per the reference. HEM's naming is misleading (reserve percentage is a different concept from cutoff).

**Impact**: Naming mismatch — HEM's documentation is imprecise. No functional bug since HEM doesn't decode the register anyway.

---

## THREE-PHASE HOLDING REGISTERS (HR 1000-1124)

These are split across two read blocks in HEM:

- `THREE_PHASE_HIGH_CONFIG_BLOCK` (HR 1000-1079, 80 regs) — largely a no-op in HEM
- `THREE_PHASE_CONFIG_BLOCK` (HR 1080-1124, 45 regs) — actively decoded

### HR 1001-1079 — High Config Block

| Address | HEM (registers.rs / decoder.rs) | GivTCP (threephase.py) | Reference (inverter_threephase.py) | Notes |
|---------|-------------------------------|------------------------|-----------------------------------|-------|
| HR 1001 | Not defined (no constant) | `set_command_save` (bool) :35 | `set_command_save` (bool) :196 | ALL AGREE — HEM block reads but no-op |
| HR 1002 | Not defined | `active_rate` (uint16) :36 | `active_rate` (uint16) :197 | ALL AGREE |
| HR 1003 | Not defined | `reactive_rate` (uint16) :37 | `reactive_rate` (uint16) :198 | ALL AGREE |
| HR 1004 | Not defined | `set_power_factor` (uint16) :38 | `set_power_factor` (uint16) :199 | ALL AGREE |
| HR 1005 | **Mentioned in comment as `REAL_TIME_CONTROL`** (registers.rs:596) but NO decoder | **NOT IN LUT** | **NOT IN LUT** (documented in code comments as three-phase mirror of HR 166) | **D1** — HEM reads block but never decodes |
| HR 1007 | Not defined | `grid_connect_time` (uint16) :39 | `grid_connect_time` (uint16) :200 | ALL AGREE |
| HR 1008 | Not defined | `grid_reconnect_time` (uint16) :40 | `grid_reconnect_time` (uint16) :201 | ALL AGREE |
| HR 1009 | Not defined | `grid_connect_slope` (deci) :41 | `grid_connect_slope` (deci) :202 | ALL AGREE |
| HR 1010 | Not defined | `com_baud_rate` (uint16) :42 | `com_baud_rate` (uint16) :203 | ALL AGREE |
| HR 1011 | Not defined | `grid_reconnect_slope` (uint16) :43 | `grid_reconnect_slope` (uint16) :204 | ALL AGREE |
| HR 1012 | Not defined | `inverter_max_power` (hexfield,0) + `battery_type` (hexfield,1) :44-45 | NOT DECODED — comment: "unverified" :205-206 | **D4** — GivTCP has hexfield here; reference says unverified |
| HR 1013 | Not defined | `Inverter_Type` (hexfield,1) :47 | NOT DECODED — comment: "needs capture" :207-208 | GivTCP has extra definition |
| HR 1017 | Not defined | `meter_fail_enable` (uint16) :48 | `meter_fail_enable` (uint16) :210 | ALL AGREE |
| HR 1018-1033 | Not defined | Grid voltage/freq limits (deci/centi) :49-64 | Grid voltage/freq limits (deci/centi) with min/max bounds :211-227 | ALL AGREE on addresses and scaling |
| HR 1034-1041 | Not defined | Time-based grid limits (centi) :65-72 | Time-based grid limits (centi) :229-236 | ALL AGREE |
| HR 1042 | Not defined | `v_10min_protect` (deci) :73 | `v_10min_protect` (deci) :237 | ALL AGREE |
| HR 1043 | Not defined | `pf_model` (uint16) :74 | `pf_model` (uint16, PowerFactorFunctionModel enum) :238 | Ref has enum type, GivTCP doesn't |
| HR 1045-1046 | Not defined | Derate params :75-76 | Derate params :239-240 | ALL AGREE |
| HR 1047-1061 | Not defined | Various PF/reactive limits :77-90 | Various :242-255 | ALL AGREE |
| HR 1063 | Not defined | `p_export_limit` (deci) :91 | `p_export_limit` (deci, max=6500) :268 | ALL AGREE on address and scaling |
| HR 1064-1067 | Not defined | Frequency derate :92-95 | Frequency derate :256-259 | ALL AGREE |
| HR 1069-1070 | Not defined | Derate params :96-97 | Derate params :261-262 | ALL AGREE |
| HR 1071-1075 | Not defined | Voltage/derate params :98-102 | Voltage/derate params :263-267 | ALL AGREE |
| HR 1077 | Not defined | Commented out `pv_input_mode` :103 | Not in LUT | Neither decodes HR 1077 |
| HR 1078 | **Mentioned as `BATTERY_RESERVE_PERCENT`** (registers.rs:498) | `battery_power_cutoff` (uint16) :104 | `battery_power_cutoff` (uint16) :269 | **D9** — HEM uses misleading name "BATTERY_RESERVE_PERCENT". GivTCP and reference agree on `battery_power_cutoff` |
| HR 1079 | Not defined | `ac_power_derate_delay` (centi) :105 | `ac_power_derate_delay` (centi) :270 | ALL AGREE |

### HR 1080-1124 — Main Config Block (actively decoded by HEM)

| Address | HEM Name (registers.rs) | HEM Decoding (decoder.rs) | GivTCP (threephase.py) | Reference (inverter_threephase.py) | Notes |
|---------|------------------------|--------------------------|------------------------|-----------------------------------|-------|
| HR 1080 | No constant defined | Not decoded | `battery_type` (uint16, BatteryType) :106 | `battery_type` (uint16, BatteryType) :272 | **D4** — HEM missing; GivTCP and reference match |
| HR 1088 | Not defined | Not decoded | `max_charge_current` (uint16) :107 | `max_charge_current` (uint16) :273 | HEM doesn't decode |
| HR 1089 | Not defined | Not decoded | `v_battery_LV` (deci, min=0.0, max=1000.0) :108 | `v_battery_lv` (deci, min=0.0, max=1000.0) :274 | HEM doesn't decode |
| HR 1090 | Not defined | Not decoded | `v_battery_CV` (deci, min=0.0, max=1000.0) :109 | `v_battery_cv` (deci, min=0.0, max=1000.0) :275 | HEM doesn't decode |
| HR 1091 | Not defined | Not decoded | `lead_acid_number` (deci) :110 | `lead_acid_number` (deci) :276 | HEM doesn't decode |
| HR 1093 | Not defined | Not decoded | `drms_enable` (uint16) :111 | `drms_enable` (bool) :277 | **MINOR**: Reference uses C.bool, GivTCP uses C.uint16 |
| HR 1098 | Not defined | Not decoded | `aging_test` (uint16) :112 | `aging_test` (uint16) :278 | HEM doesn't decode |
| HR 1100 | Not defined | Not decoded | `bypass_enable` (uint16) :113 | `bypass_enable` (bool) :279 | **MINOR**: Reference uses C.bool, GivTCP uses C.uint16 |
| HR 1101 | Not defined | Not decoded | `npe_enable` (uint16) :114 | `npe_enable` (bool) :280 | **MINOR**: Reference uses C.bool, GivTCP uses C.uint16 |
| HR 1104 | Not defined | Not decoded | `unbalance_output_enable` (bool) :115 | `unbalance_output_enable` (bool) :281 | ALL AGREE on converter |
| HR 1105 | Not defined | Not decoded | `backup_enable` (uint16, Enable) :116 | `backup_enable` (bool) :282 | **MINOR**: Reference uses C.bool, GivTCP uses Enable enum |
| HR 1106 | Not defined | Not decoded | `v_backup_nominal` (nominal_voltage) :117 | `v_backup_nominal` (nominal_voltage) :283 | ALL AGREE |
| HR 1107 | Not defined | Not decoded | `f_backup_nominal` (nominal_frequency) :118 | `f_backup_nominal` (nominal_frequency) :284 | ALL AGREE |
| **HR 1108** | `HR_3PH_BATTERY_DISCHARGE_LIMIT` (registers.rs:324) | `snap.discharge_rate` (decoder.rs:1002) | `battery_discharge_limit_ac` (uint16) :119 | `battery_discharge_limit_ac` (uint16) :286 | ✅ ALL AGREE — address, type, purpose |
| **HR 1109** | `HR_3PH_BATTERY_SOC_RESERVE` (registers.rs:326) | `snap.battery_reserve` clamped 4-100 (decoder.rs:1003) | `battery_soc_reserve` (uint16) :120 | `battery_soc_reserve` (uint16) :288 | ✅ ALL AGREE — HEM clamps 4-100, neither Python lib has bounds |
| **HR 1110** | `HR_3PH_BATTERY_CHARGE_LIMIT` (registers.rs:328) | `snap.charge_rate` (decoder.rs:1005) | `battery_charge_limit_ac` (uint16) :121 | `battery_charge_limit_ac` (uint16) :290 | ✅ ALL AGREE |
| **HR 1111** | `HR_3PH_CHARGE_TARGET_SOC` (registers.rs:330) | `snap.target_soc` clamped 4-100 (decoder.rs:1006) | `charge_target_soc` (uint16) :122 | `charge_target_soc` (uint16) :292 | ✅ ALL AGREE |
| **HR 1112** | `HR_3PH_AC_CHARGE_ENABLE` (registers.rs:332) | `snap.enable_charge = HR1112 \|\| HR1123` (decoder.rs:1017) | `ac_charge_enable` (uint16, Enable) :123 | `ac_charge_enable` (bool) :295 | HEM ORs with force_charge; Reference uses C.bool, GivTCP uses Enable enum |
| **HR 1113** | `HR_3PH_CHARGE_SLOT_1_START` (registers.rs:334) | `snap.charge_slots[0]` via decode_timeslot (decoder.rs:1010) | `charge_slot_1` (timeslot) + individual `start`/`end` :124-126 | `charge_slot_1` (timeslot) :297 | ✅ ALL AGREE |
| **HR 1114** | `HR_3PH_CHARGE_SLOT_1_END` (registers.rs:335) | Same as above :1010 | `charge_slot_1_end` (uint16, valid 0-2359) :126 | — (part of timeslot) :297 | ✅ ALL AGREE |
| **HR 1115** | `HR_3PH_CHARGE_SLOT_2_START` (registers.rs:337) | `snap.charge_slots[1]` :1011 | `charge_slot_2` :127-129 | `charge_slot_2` :299 | ✅ ALL AGREE |
| **HR 1116** | `HR_3PH_CHARGE_SLOT_2_END` (registers.rs:338) | Same :1011 | `charge_slot_2_end` :129 | — :299 | ✅ ALL AGREE |
| HR 1117 | Not in HEM registers.rs | Not decoded | `load_compensation_enable` (uint16, Enable) :130 | `load_compensation_enable` (bool) :300 | HEM doesn't decode |
| **HR 1118** | `HR_3PH_DISCHARGE_SLOT_1_START` (registers.rs:340) | `snap.discharge_slots[0]` :1012 | `discharge_slot_1` :131-133 | `discharge_slot_1` :302 | ✅ ALL AGREE |
| **HR 1119** | `HR_3PH_DISCHARGE_SLOT_1_END` (registers.rs:341) | Same :1012 | `discharge_slot_1_end` :133 | — :302 | ✅ ALL AGREE |
| **HR 1120** | `HR_3PH_DISCHARGE_SLOT_2_START` (registers.rs:343) | `snap.discharge_slots[1]` :1013 | `discharge_slot_2` :134-136 | `discharge_slot_2` :304 | ✅ ALL AGREE |
| **HR 1121** | `HR_3PH_DISCHARGE_SLOT_2_END` (registers.rs:344) | Same :1013 | `discharge_slot_2_end` :136 | — :304 | ✅ ALL AGREE |
| **HR 1122** | `HR_3PH_FORCE_DISCHARGE_ENABLE` (registers.rs:346) | `snap.enable_discharge` (decoder.rs:1018) | `enable_discharge` (uint16, Enable) :137 | `force_discharge_enable` (bool) :307 | 🟡 HEM and GivTCP call it "enable_discharge"; Reference uses "force_discharge_enable". All at same address. GivTCP naming misleads — this is NOT the same as single-phase HR(59) enable_discharge. |
| **HR 1123** | `HR_3PH_FORCE_CHARGE_ENABLE` (registers.rs:348) | OR'd into `snap.enable_charge` (decoder.rs:1017) | `force_charge_enable` (uint16, Enable) :138 | `force_charge_enable` (bool) :308 | ✅ ALL AGREE |
| HR 1124 | **MISSING** (no constant defined) | **NOT DECODED** | `battery_maintenance_mode` (uint16, BatteryMaintenance) :139 | `battery_maintenance_mode` (uint16, BatteryMaintenance) :309 | **D5** — HEM neither defines nor decodes HR 1124 |

---

## THREE-PHASE INPUT REGISTERS (IR 1000-1413)

Seven poll blocks in HEM (registers.rs:654-662). Each block header below shows the HEM block name, decoder function, and register range.

### Block 1: IR 1000-1059 — PV Measurements (decode_input_1000_1059)

| Address (offset from 1000) | HEM Decoding (decoder.rs) | GivTCP (threephase.py) | Reference (inverter_threephase.py) | Notes |
|---------------------------|--------------------------|------------------------|-----------------------------------|-------|
| IR 1001 (offset 1) | `snap.pv1_voltage = val * 0.1` :1043 | `v_pv1` (deci, = /10) :144 | `v_pv1` (deci, min=0.0, max=2000.0) :314 | ✅ ALL AGREE scaling |
| IR 1002 (offset 2) | `snap.pv2_voltage = val * 0.1` :1044 | `v_pv2` (deci) :145 | `v_pv2` (deci, min=0.0, max=2000.0) :315 | ✅ ALL AGREE |
| IR 1009 (offset 9) | `snap.pv1_current = val * 0.1` :1045 | `i_pv1` (deci) :146 | `i_pv1` (deci, min=0.0, max=500.0) :317 | ✅ ALL AGREE |
| IR 1010 (offset 10) | `snap.pv2_current = val * 0.1` :1046 | `i_pv2` (deci) :147 | `i_pv2` (deci, min=0.0, max=500.0) :318 | ✅ ALL AGREE |
| IR 1017-1018 (offset 17-18) | `snap.pv1_power = uint32 * 0.1` :1047-1049 | `p_pv1` (uint32, deci) :148 | `p_pv1` (uint32, deci, max=100000) :320 | ✅ ALL AGREE — uint32 ÷10 |
| IR 1019-1020 (offset 19-20) | `snap.pv2_power = uint32 * 0.1` :1048-1050 | `p_pv2` (uint32, deci) :149 | `p_pv2` (uint32, deci, max=100000) :321 | ✅ ALL AGREE |

### Block 2: IR 1060-1119 — Grid, Load, Inverter Output (decode_input_1060_1119)

| Address (offset from 1060) | HEM Decoding (decoder.rs) | GivTCP (threephase.py) | Reference (inverter_threephase.py) | Notes |
|---------------------------|--------------------------|------------------------|-----------------------------------|-------|
| IR 1061 (offset 1) | `v1 = val * 0.1` → `snap.grid_voltage = v1` :1078-1081 | `v_ac1` (deci) :153 | `v_ac1` (deci, min=0.0, max=500.0) :326 | ✅ ALL AGREE |
| IR 1062 (offset 2) | `v2 = val * 0.1` (used for max) :1079 | `v_ac2` (deci) :154 | `v_ac2` (deci, min=0.0, max=500.0) :327 | ✅ ALL AGREE |
| IR 1063 (offset 3) | `v3 = val * 0.1` (used for max) :1080 | `v_ac3` (deci) :155 | `v_ac3` (deci, min=0.0, max=500.0) :328 | ✅ ALL AGREE |
| IR 1064 (offset 4) | `i1 = val * 0.1` (meter calcs) :1086 | `i_ac1` (deci) :156 | `i_ac1` (deci, min=0.0, max=500.0) :329 | ✅ ALL AGREE |
| IR 1065 (offset 5) | `i2 = val * 0.1` :1087 | `i_ac2` (deci) :157 | `i_ac2` (deci) :330 | ✅ ALL AGREE |
| IR 1066 (offset 6) | `i3 = val * 0.1` :1088 | `i_ac3` (deci) :158 | `i_ac3` (deci) :331 | ✅ ALL AGREE |
| IR 1067 (offset 7) | `snap.grid_frequency = val * 0.01` :1082 | `f_ac1` (centi) :159 | `f_ac1` (centi, min=40.0, max=70.0) :333 | ✅ ALL AGREE scaling |
| IR 1068 (offset 8) | `pf_raw = signed(val) * 0.001` :1104 | `power_factor` (int16) :160 | `power_factor` (int16) :335 | ✅ ALL AGREE — int16 ÷1000 |
| IR 1069-1070 (offset 9-10) | Commented but NOT stored in snapshot :1070 | `p_inverter_out` (int32, deci) :161 | `p_inverter_out` (int32, deci, min=-100000) :336 | HEM doesn't store inverter_out power |
| IR 1071-1072 (offset 11-12) | Not decoded | `p_inverter_ac_charge` (uint32, deci) :162 | `p_inverter_ac_charge` (uint32, deci) :337 | HEM doesn't decode |
| IR 1073-1074 (offset 13-14) | `p_apparent = uint32 * 0.1` → meter :1106 | `p_grid_apparent` (uint32, deci) :163 | `p_grid_apparent` (uint32, deci) :339 | ✅ ALL AGREE |
| IR 1075 (offset 15) | Not decoded | `system_mode` (C.bool, SystemMode) :164 | `system_mode` (C.uint16) :341 | **D7** — GivTCP uses wrong converter (bool) |
| IR 1076 (offset 16) | Not decoded | `status` (uint16, Status) :165 | `status` (uint16, Status) :343 | HEM skips |
| IR 1077 (offset 17) | Not decoded | `start_delay_time` (uint16) :166 | `start_delay_time` (uint16) :344 | HEM skips |
| IR 1079-1080 (offset 19-20) | `p_import = uint32 * 0.1` :1091 | `p_meter_import` (uint32, deci) :167 | `p_meter_import` (uint32, deci) :345 | ✅ ALL AGREE |
| IR 1081-1082 (offset 21-22) | `p_export = uint32 * 0.1` :1092 | `p_meter_export` (uint32, deci) :168 | `p_meter_export` (uint32, deci) :346 | ✅ ALL AGREE |
| IR 1083 (offset 23) | Comment mentions p_load_ac1 but not stored :1074 | `p_load_ac1` (deci, max=6500) :169 | `p_load_ac1` (deci, max=6500) :347 | HEM mentions only in comments |
| IR 1084 (offset 24) | Not stored | `p_load_ac2` (deci) :170 | `p_load_ac2` (deci) :348 | HEM skips |
| IR 1085 (offset 25) | Not stored | `p_load_ac3` (deci) :171 | `p_load_ac3` (deci) :349 | HEM skips |
| **IR 1089-1090** (offset 29-30) | `snap.home_power = uint32 * 0.1` :1097 | `p_load_all` (uint32, deci) :172 | `p_load_all` (uint32, deci, max=100000) :350 | ✅ ALL AGREE — authoritative home power |
| IR 1091-1093 (offset 31-33) | Not decoded :1118-1120 | `p_out_ac1/2/3` (deci) :173-175 | `p_out_ac1/2/3` (deci) :351-353 | HEM reads but doesn't store (unsigned export-only, not net) |
| IR 1094-1096 (offset 34-36) | Not decoded | `v_out_ac1/2/3` (deci) :176-178 | `v_out_ac1/2/3` (deci) :354-356 | HEM skips |

**Grid power derivation**: HEM computes `snap.grid_power = (p_export - p_import)` :1093, where both are unsigned uint32 ÷10. This means positive = exporting (net export). Same approach as single-phase.

### Block 3: IR 1120-1179 — Battery (decode_input_1120_1179)

| Address (offset from 1120) | HEM Decoding (decoder.rs) | GivTCP (threephase.py) | Reference (inverter_threephase.py) | Notes |
|---------------------------|--------------------------|------------------------|-----------------------------------|-------|
| IR 1120 (offset 0) | Not decoded | `battery_priority` (uint16, BatteryPriority) :184 | `battery_priority` (uint16) :360 | HEM skips |
| IR 1121 (offset 1) | Not decoded | `battery_type` (int16) :185 | `battery_type_ir` (uint16, BatteryType) :362 | **D6** — HEM doesn't decode; GivTCP uses int16 vs reference uint16 |
| IR 1124 (offset 4) | Not decoded | `dc_status` (uint16, Status) :186 | `dc_status` (uint16, Status) :363 | HEM skips |
| IR 1128 (offset 8) | `snap.inverter_temperature = val * 0.1` :1144 | `t_inverter` (deci) :187 | `t_inverter` (deci, min=-60.0, max=150.0) :364 | ✅ ALL AGREE |
| IR 1129 (offset 9) | Not decoded | `t_boost` (deci) :188 | `t_boost` (deci) :365 | HEM skips |
| IR 1130 (offset 10) | Not decoded | `t_buck_boost` (deci) :189 | `t_buck_boost` (deci) :366 | HEM skips |
| IR 1131 (offset 11) | `snap.battery_voltage = val * 0.1` :1145 | `v_battery_bms` (deci) :190 | `v_battery_bms` (deci, min=0.0, max=1000.0) :367 | ✅ ALL AGREE — ÷10 V |
| IR 1132 (offset 12) | `snap.soc = val as u8` :1146 | `battery_soc` (uint16) :191 | `battery_soc` (uint16, min=0, max=100) :369 | ✅ ALL AGREE |
| IR 1133 (offset 13) | Not decoded | `v_battery_pcs` (deci) :192 | `v_battery_pcs` (deci) :370 | HEM skips |
| IR 1134 (offset 14) | Not decoded | `v_dc_bus` (deci) :193 | `v_dc_bus` (deci) :371 | HEM skips |
| IR 1135 (offset 15) | Not decoded | `v_inv_bus` (deci) :194 | `v_inv_bus` (deci) :372 | HEM skips |
| **IR 1136-1137** (offset 16-17) | `p_discharge = uint32 * 0.1` :1148 | `p_battery_discharge` (uint32, deci) :195 | `p_battery_discharge` (uint32, deci, max=100000) :373 | ✅ ALL AGREE |
| **IR 1138-1139** (offset 18-19) | `p_charge = uint32 * 0.1` :1149 | `p_battery_charge` (uint32, deci) :196 | `p_battery_charge` (uint32, deci, max=100000) :374 | ✅ ALL AGREE |
| **IR 1140** (offset 20) | `snap.battery_current = signed(val) * 0.1` :1153 | `i_battery` (int16, deci) :197 | `i_battery` (int16, deci, min=-500.0, max=500.0) :376 | ✅ ALL AGREE |

#### CRITICAL: Battery power sign convention (three-phase)

All three codebases use the SAME convention:

- **Reference** (`inverter_threephase.py:373-374`): `p_battery_discharge` and `p_battery_charge` are BOTH unsigned uint32 ÷10. No direct net battery_power register.
- **GivTCP** (`threephase.py:195-196`): Same — separate unsigned discharge/charge registers.
- **HEM** (`decoder.rs:1148-1151`): Derives `snap.battery_power = (p_discharge - p_charge)` — positive = discharging. This is correct and consistent with the convention documented at `decoder.rs:1150`: "Our convention (matches references): positive = discharging."

Neither GivTCP nor the reference provide a single combined `battery_power` field in their LUTs. The net calculation (discharge minus charge) is the de facto standard used by HEM, which is semantically correct but not one-for-one comparable to either Python library at the register-LUT level.

### Block 4: IR 1180-1239 — EPS (decode_input_1180_1239)

| Address (offset from 1180) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1180-1192 (offset 0-12) | **NO-OP** — decoder.rs:1157-1160: "Not yet exposed in InverterSnapshot" | `f_nominal_eps` (centi), `v_eps_ac1/2/3`, `i_eps_ac1/2/3`, `p_eps_ac1/2/3` (uint32, deci) :202-211 | Same as GivTCP :380-389 | **HEM reads the block but discards all data** |

### Block 5: IR 1240-1299 — Additional Power Meters (decode_input_1240_1299)

| Address (offset from 1240) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1240-1241 (offset 0-1) | Not decoded (comment mentions it) :1164 | `p_export` (uint32, deci) :216 | `p_export` (uint32, deci) :393 | HEM reads but doesn't store |
| IR 1244-1245 (offset 4-5) | `p_meter2 = uint32 * 0.1` → second CT meter :1166 | `p_meter2` (uint32, deci) :217 | `p_meter2` (uint32, deci) :394 | ✅ ALL AGREE |

### Block 6: IR 1300-1359 — Fault Codes + Firmware (decode_input_1300_1359)

| Address (offset from 1300) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1300-1307 (offset 0-7) | Not decoded | Fault codes 0-7 via `inverter_fault_code2` :222-229 | Fault codes 0-7 via `_inverter_fault_code2` :398-405 | **HEM reads block but does NOT decode fault codes** |
| IR 1317-1319 (offset 17-19) | Not decoded | `tph_software_version` (string, 3 regs) :235 | `tph_software_version` (string, 3 regs) :409 | HEM reads but doesn't decode software version string |
| IR 1320-1324 (offset 20-24) | `snap.firmware_version = decode_serial(data, 20, 5)` :1202 | `tph_firmware_version` (string, 5 regs) :233 | `tph_firmware_version` (string, 5 regs) :410 | ✅ ALL AGREE — 5-register ASCII string |
| IR 1325 (offset 25) | `snap.dsp_firmware_version = format!("{}", val)` :1211 | `ac_dsp_firmware_version` (string) :231 | `ac_dsp_firmware_version` (uint16) :411 | **MINOR**: GivTCP uses C.string, Reference uses C.uint16 |
| IR 1326 (offset 26) | `snap.dc_dsp_firmware_version = format!("{}", val)` :1216 | `dc_dsp_firmware_version` (string) :232 | `dc_dsp_firmware_version` (uint16) :412 | Same as above |
| IR 1327 (offset 27) | Not stored separately | `tph_arm_firmware_version` (string) :230 | `tph_arm_firmware_version` (uint16) :413 | HEM doesn't store ARM fw separately |

### Block 7: IR 1360-1413 — Energy Totals (decode_input_1360_1413)

| Address (offset from 1360) | HEM Decoding ✓ | GivTCP | Reference | Notes |
|---------------------------|----------------|--------|-----------|-------|
| IR 1360-1361 (offset 0-1) | `e_inverter_out_today` — not stored in snapshot | `e_inverter_out_today` (uint32, deci) :239 | `e_inverter_out_today` (uint32, deci) :419 | ALL AGREE on register/scale |
| IR 1362-1363 (offset 2-3) | Not decoded | `e_inverter_out_total` (uint32, deci) :240 | `e_inverter_out_total` (uint32, deci) :420 | HEM skips |
| IR 1366-1367 (offset 6-7) | `e_pv1_today` — NOT stored separately, used for fallback sum :1240 | `e_pv1_today` (uint32, deci) :241 | `e_pv1_today` (uint32, deci) :421 | ALL AGREE |
| IR 1368-1369 (offset 8-9) | Not decoded | `e_pv1_total` (uint32, deci) :242 | `e_pv1_total` (uint32, deci) :422 | HEM skips |
| IR 1370-1371 (offset 10-11) | `e_pv2_today` — NOT stored separately, used for fallback sum :1240 | `e_pv2_today` (uint32, deci) :243 | `e_pv2_today` (uint32, deci) :423 | ALL AGREE |
| IR 1372-1373 (offset 12-13) | Not decoded | `e_pv2_total` (uint32, deci) :244 | `e_pv2_total` (uint32, deci) :424 | HEM skips |
| IR 1374-1375 (offset 14-15) | `snap.total_solar_kwh` via `decode_lifetime_total_kwh` :1259 | `e_pv_total` (uint32, deci) :245 | `e_pv_total` (uint32, deci) :425 | ✅ ALL AGREE |
| **IR 1376-1377** (offset 16-17) | `snap.today_ac_charge_kwh` :1253 | `e_ac_charge_today` (uint32, deci) :246 | `e_ac_charge_today` (uint32, deci) :426 | ✅ ALL AGREE — correctly labeled as AC charge (not consumption) |
| IR 1378-1379 (offset 18-19) | Not decoded | `e_ac_charge_total` (uint32, deci) :247 | `e_ac_charge_total` (uint32, deci) :427 | HEM skips |
| **IR 1380-1381** (offset 20-21) | `snap.today_import_kwh` :1244 | `e_import_today` (uint32, deci) :248 | `e_import_today` (uint32, deci) :428 | ✅ ALL AGREE |
| **IR 1382-1383** (offset 22-23) | `snap.total_import_kwh` via `decode_lifetime_total_kwh` :1254 | `e_import_total` (uint32, deci) :249 | `e_import_total` (uint32, deci) :429 | ✅ ALL AGREE |
| **IR 1384-1385** (offset 24-25) | `snap.today_export_kwh` :1245 | `e_export_today` (uint32, deci) :250 | `e_export_today` (uint32, deci) :430 | ✅ ALL AGREE |
| **IR 1386-1387** (offset 26-27) | `snap.total_export_kwh` via `decode_lifetime_total_kwh` :1255 | `e_export_total` (uint32, deci) :251 | `e_export_total` (uint32, deci) :431 | ✅ ALL AGREE |
| **IR 1388-1389** (offset 28-29) | `snap.today_discharge_kwh` :1247 | `e_battery_discharge_today` (uint32, deci) :252 | `e_battery_discharge_today` (uint32, deci) :432 | ✅ ALL AGREE |
| **IR 1390-1391** (offset 30-31) | `snap.total_discharge_kwh` via `decode_lifetime_total_kwh` :1257 | `e_battery_discharge_total` (uint32, deci) :253 | `e_battery_discharge_total` (uint32, deci) :433 | ✅ ALL AGREE |
| **IR 1392-1393** (offset 32-33) | `snap.today_charge_kwh` :1246 | `e_battery_charge_today` (uint32, deci) :254 | `e_battery_charge_today` (uint32, deci) :434 | ✅ ALL AGREE |
| **IR 1394-1395** (offset 34-35) | `snap.total_charge_kwh` via `decode_lifetime_total_kwh` :1256 | `e_battery_charge_total` (uint32, deci) :255 | `e_battery_charge_total` (uint32, deci) :435 | ✅ ALL AGREE |
| **IR 1396-1397** (offset 36-37) | `snap.today_consumption_kwh` :1248 | `e_load_today` (uint32, deci) :256 | `e_load_today` (uint32, deci) :436 | ✅ ALL AGREE — native load consumption register |
| IR 1398-1399 (offset 38-39) | Not decoded | `e_load_total` (uint32, deci) :257 | `e_load_total` (uint32, deci) :437 | HEM skips |
| IR 1400-1403 (offset 40-43) | Not decoded | `e_export2_today/total` (uint32, deci) :258-259 | `e_export2_today/total` (uint32, deci) :438-439 | HEM skips |
| **IR 1412-1413** (offset 52-53) | `snap.today_solar_kwh` (preferred, with fallback) :1236-1243 | `e_pv_today` (uint32, deci) :260 | `e_pv_today` (uint32, deci) :440 | ✅ ALL AGREE — primary solar today source |

**Verification of energy total scaling**: All three codebases consistently use `uint32 ÷ 10 → kWh` (deci-scaling) for every energy register in this block. There are NO scaling discrepancies.

**HEM solar today logic** (decoder.rs:1236-1243): Prefers IR(1412-1413) `e_pv_today`. Falls back to sum of `e_pv1_today` (IR 1366-1367) + `e_pv2_today` (IR 1370-1371) when the aggregate is zero. This is a HEM-specific enhancement not present in either Python library.

---

## GATEWAY INPUT REGISTERS (IR 1600-1859)

Five poll blocks in HEM (registers.rs:702-745). The reference library has TWO variants (GatewayV1RegisterGetter and GatewayV2RegisterGetter) differing in uint32 byte order and AIO serial addresses.

### Block 1: IR 1600-1659 — Identity, Power, Faults, Daily/Lifetime Energy (decode_gateway_1600_1659)

| Address (offset from 1600) | HEM Decoding | GivTCP (gateway.py) | Reference (gateway.py) | Notes |
|---------------------------|--------------|---------------------|------------------------|-------|
| IR 1600-1603 (offset 0-3) | `snap.gateway_software_version` (gateway_version encoding) :1687 | `software_version` (gateway_version) :33 | `software_version` (gateway_version) :67 | ✅ ALL AGREE |
| IR 1603 (offset 3) | `snap.gateway_is_v2 = val >= 10` :1688 | Used for serial selection (read.py:1683) :1683 | `select_gateway` uses IR(1603) >= 10 :230-232 | ✅ ALL AGREE on V1/V2 detection threshold |
| IR 1604 (offset 4) | `snap.gateway_work_mode` :1690 | `work_mode` (uint16, WorkMode) :34 | `work_mode` (uint16, WorkMode) :68 | ✅ ALL AGREE |
| IR 1608 (offset 8) | `snap.grid_voltage = val * 0.1` :1701 | `v_grid` (int16, deci) :38 | `v_grid` (int16, deci) :69 | ✅ ALL AGREE — signed int16 ÷10 |
| IR 1609 (offset 9) | Not decoded | `i_grid` (int16, deci) :39 | `i_grid` (int16, deci) :70 | HEM skips |
| IR 1610 (offset 10) | Not decoded | `v_load` (deci) :40 | `v_load` (deci) :71 | HEM skips |
| IR 1611 (offset 11) | Not decoded | `i_load` (deci) :41 | `i_load` (deci) :72 | HEM skips |
| IR 1612 (offset 12) | `snap.pv1_current = signed(val) * 0.1` :1728 | `i_pv` (int16, deci) :42 | `i_pv` (int16, deci) :73 | ✅ ALL AGREE |
| IR 1616 (offset 16) | Not decoded | `p_ac1` (int16) :43 | `p_ac1` (int16) :74 | HEM skips |
| IR 1617 (offset 17) | `snap.solar_power = val` (unsigned) :1731-1733 | `p_pv` (uint16) :44 | `p_pv` (uint16) :75 | ✅ ALL AGREE — unsigned uint16 W |
| IR 1618 (offset 18) | `snap.home_power = val` (unsigned) :1738 | `p_load` (uint16) :45 | `p_load` (uint16) :76 | ✅ ALL AGREE — unsigned, excludes EV |
| IR 1619 (offset 19) | Not decoded | `p_liberty` (int16) :46 | `p_liberty` (int16) :77 | HEM skips |
| IR 1620-1621 (offset 20-21) | Not decoded | `fault_protection` (uint32) :47 | `fault_protection` (uint32) :78 | HEM skips |
| IR 1622-1623 (offset 22-23) | `snap.gateway_fault_codes` via fault-bitmask decoder :1694-1695 | `gateway_fault_codes` (uint32, gateway_fault_code) :48 | `gateway_fault_codes` (uint32, _gateway_fault_code) :79 | ✅ ALL AGREE — 32-bit MSB-first bitmask, same fault names |
| IR 1624 (offset 24) | Not decoded | `v_grid_relay` (deci) :49 | `v_grid_relay` (deci) :80 | HEM skips |
| IR 1625 (offset 25) | Not decoded | `v_inverter_relay` (deci) :50 | `v_inverter_relay` (deci) :81 | HEM skips |
| IR 1627-1631 (offset 27-31) | `snap.first_inverter_serial` :1691 | `first_inverter_serial_number` (string) :51 | `first_inverter_serial_number` (serial) :82 | ✅ ALL AGREE — 5-register Latin-1 |
| IR 1640 (offset 40) | `snap.today_import_kwh = val * 0.1` :1741 | `e_grid_import_today` (deci) :52 | `e_grid_import_today` (deci) :86 | ✅ ALL AGREE |
| **IR 1641-1642** (offset 41-42) | `snap.total_import_kwh` via `gw_u32` (V1/V2-aware) :1754 | `e_grid_import_total` (uint32, deci — V1 ONLY) :53 | V1: (1641,1642) :124, V2: (1642,1641) :142 | **D3** ⚠️ GivTCP always V1; HEM handles both |
| IR 1643 (offset 43) | `snap.today_solar_kwh = val * 0.1` :1742 | `e_pv_today` (deci) :54 | `e_pv_today` (deci) :87 | ✅ ALL AGREE |
| **IR 1644-1645** (offset 44-45) | `total_solar` via `gw_u32` (not stored separately) | `e_pv_total` (uint32, deci — V1 ONLY) :55 | V1: (1644,1645) :125, V2: (1645,1644) :143 | **D3** ⚠️ |
| IR 1646 (offset 46) | `snap.today_export_kwh = val * 0.1` :1743 | `e_grid_export_today` (deci) :56 | `e_grid_export_today` (deci) :88 | ✅ ALL AGREE |
| **IR 1647-1648** (offset 47-48) | `snap.total_export_kwh` via `gw_u32` :1755 | `e_grid_export_total` (uint32, deci — V1 ONLY) :57 | V1: (1647,1648) :127, V2: (1648,1647) :145 | **D3** ⚠️ |
| IR 1649 (offset 49) | `snap.today_charge_kwh = val * 0.1` :1744 | `e_aio_charge_today` (deci) :61 | `e_aio_charge_today` (deci) :89 | ✅ ALL AGREE |
| **IR 1650-1651** (offset 50-51) | `snap.total_charge_kwh` via `gw_u32` :1756 | `e_aio_charge_total` (uint32, deci — V1 ONLY) :62 | V1: (1650,1651) :128, V2: (1651,1650) :146 | **D3** ⚠️ |
| IR 1652 (offset 52) | `snap.today_discharge_kwh = val * 0.1` :1745 | `e_aio_discharge_today` (deci) :63 | `e_aio_discharge_today` (deci) :90 | ✅ ALL AGREE |
| **IR 1653-1654** (offset 53-54) | `snap.total_discharge_kwh` via `gw_u32` :1757 | `e_aio_discharge_total` (uint32, deci — V1 ONLY) :64 | V1: (1653,1654) :129, V2: (1654,1653) :147 | **D3** ⚠️ |
| IR 1655 (offset 55) | `snap.today_consumption_kwh` :1746 | `e_load_today` (deci) :58 | `e_load_today` (deci) :91 | ✅ ALL AGREE |
| **IR 1656-1657** (offset 56-57) | Not stored (load total not in snapshot) | `e_load_total` (uint32, deci — V1 ONLY) :59 | V1: (1656,1657) :130, V2: (1657,1656) :147 | HEM skips load total; GivTCP V1-only |

### Block 2: IR 1660-1719 — AIO Summary + Per-AIO Charge (decode_gateway_1660_1719)

| Address (offset from 1660) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1700 (offset 40) | `snap.parallel_aio_count` :1787 | `parallel_aio_num` (uint16) :96 | `parallel_aio_num` (uint16) :95 | ✅ ALL AGREE |
| IR 1701 (offset 41) | `snap.parallel_aio_online` :1788 | `parallel_aio_online_num` (uint16) :97 | `parallel_aio_online_num` (uint16) :96 | ✅ ALL AGREE |
| **IR 1702** (offset 42) | `snap.battery_power = -signed(val)` :1796-1797 | `p_aio_total` (int16) — negated in read.py :1554 | `p_aio_total` (int16) :97 | **SIGN CONVENTION** ⬇️ |

#### CRITICAL: Gateway battery power sign convention

All three codebases AGREE on the sign convention and necessary negation:

- **Wire convention**: IR(1702) `p_aio_total` is SIGNED int16 where **positive = charging** (AIO consuming power from grid).
- **HEM** (decoder.rs:1796): `snap.battery_power = -signed(get_reg(data, 42))` — **negates** to get positive=discharging for internal consistency.
- **GivTCP** (read.py:1554): `Battery_Power = -GEInv.p_aio_total` — **negates** identically.
- **Reference**: `gateway.py:97` — `p_aio_total` is defined as `Def(C.int16, None, IR(1702))` but the reference library does not negate in the LUT; the calling application layer is expected to handle the sign reversal.

✅ No discrepancy — all three agree the raw value must be negated.

| Address (offset from 1660) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1703 (offset 43) | Not decoded | `aio_state` (uint16, State) :66 | `aio_state` (uint16, State) :98 | HEM skips |
| IR 1704 (offset 44) | Not decoded | `battery_firmware_version` (int16) :67 | `battery_firmware_version` (uint16) :99 | **MINOR**: GivTCP uses int16, ref uses uint16 |
| IR 1705 (offset 45) | `per_aio_charge_today_kwh[0]` :1809 | `e_aio1_charge_today` (deci) :69 | `e_aio1_charge_today` (deci) :103 | ✅ ALL AGREE |
| IR 1706-1707 (offset 46-47) | Not decoded | `e_aio1_charge_total` (uint32, deci) :70 | V1: (1706,1707) :131 | HEM doesn't store per-AIO totals |
| IR 1708 (offset 48) | `per_aio_charge_today_kwh[1]` :1810 | `e_aio2_charge_today` (deci) :78 | `e_aio2_charge_today` (deci) :104 | ✅ ALL AGREE |
| IR 1711 (offset 51) | `per_aio_charge_today_kwh[2]` :1811 | `e_aio3_charge_today` (deci) :87 | `e_aio3_charge_today` (deci) :105 | ✅ ALL AGREE |

### Block 3: IR 1720-1779 — Per-AIO Discharge (decode_gateway_1720_1779)

| Address (offset from 1720) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1750 (offset 30) | `per_aio_discharge_today_kwh[0]` :1818 | `e_aio1_discharge_today` (deci) :71 | `e_aio1_discharge_today` (deci) :106 | ✅ ALL AGREE |
| IR 1753 (offset 33) | `per_aio_discharge_today_kwh[1]` :1819 | `e_aio2_discharge_today` (deci) :80 | `e_aio2_discharge_today` (deci) :107 | ✅ ALL AGREE |
| IR 1756 (offset 36) | `per_aio_discharge_today_kwh[2]` :1820 | `e_aio3_discharge_today` (deci) :89 | `e_aio3_discharge_today` (deci) :108 | ✅ ALL AGREE |

### Block 4: IR 1780-1830 — SOC, Aggregate Energy, Per-AIO Inverter Power (decode_gateway_1780_1830)

| Address (offset from 1780) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1795 (offset 15) | Not decoded | `e_battery_charge_today` (deci) :100 | `e_battery_charge_today` (deci) :112 | HEM skips |
| IR 1796-1797 (offset 16-17) | Not decoded | `e_battery_charge_total` (uint32, deci) :101 | V1: (1796,1797) :136 | HEM skips |
| IR 1798 (offset 18) | Not decoded | `e_battery_discharge_today` (deci) :102 | `e_battery_discharge_today` (deci) :113 | HEM skips |
| IR 1799-1800 (offset 19-20) | Not decoded | `e_battery_discharge_total` (uint32, deci) :103 | V1: (1799,1800) :137 | HEM skips |
| IR 1801 (offset 21) | `snap.per_aio_soc[0]` :1828 | `aio1_soc` (uint16) :73 | `aio1_soc` (uint16) :114 | ✅ ALL AGREE |
| IR 1802 (offset 22) | `snap.per_aio_soc[1]` :1829 | `aio2_soc` (uint16) :82 | `aio2_soc` (uint16) :115 | ✅ ALL AGREE |
| IR 1803 (offset 23) | `snap.per_aio_soc[2]` :1830 | `aio3_soc` (uint16) :91 | `aio3_soc` (uint16) :116 | ✅ ALL AGREE |
| **IR 1816** (offset 36) | `snap.per_aio_power[0] = -signed(val)` :1854 | `p_aio1_inverter` (int16) :74 | `p_aio1_inverter` (int16) :117 | **NEGATED** — all agree |

#### Gateway per-AIO power sign convention

All three codebases agree the per-AIO inverter power must be negated:

- **HEM** (decoder.rs:1854): `-signed(get_reg(data, 36))` — negates.
- **GivTCP** (read.py:1682): `Invertor_Power = -GEInv.p_aio1_inverter` — negates.
- **Reference**: `gateway.py:117-119` — `p_aio1_inverter` is `Def(C.int16, None, IR(1816))` with no negation in the LUT (application-level concern).

✅ No discrepancy.

| Address (offset from 1780) | HEM Decoding | GivTCP | Reference | Notes |
|---------------------------|--------------|--------|-----------|-------|
| IR 1817 (offset 37) | `snap.per_aio_power[1] = -signed(val)` :1855 | `p_aio2_inverter` (int16) :83 | `p_aio2_inverter` (int16) :118 | ✅ ALL AGREE (negated) |
| IR 1818 (offset 38) | `snap.per_aio_power[2] = -signed(val)` :1856 | `p_aio3_inverter` (int16) :92 | `p_aio3_inverter` (int16) :119 | ✅ ALL AGREE (negated) |

### Block 5: IR 1831-1859 — Per-AIO Serials (decode_gateway_1831_1859)

| Variant | HEM | GivTCP | Reference | Notes |
|---------|-----|--------|-----------|-------|
| **V1 AIO1 serial** | `decode_serial(data, 0, 5)` (IR 1831-1835) :1872 | `aio1_serial_number` :75 | `aio1_serial_number` (1831-1835) :160 | ✅ ALL AGREE |
| **V1 AIO2 serial** | `decode_serial(data, 7, 5)` (IR 1838-1842) :1873 | `aio2_serial_number` :84 | `aio2_serial_number` (1838-1842) :161 | ✅ ALL AGREE |
| **V1 AIO3 serial** | `decode_serial(data, 14, 5)` (IR 1845-1849) :1873 | `aio3_serial_number` :93 | `aio3_serial_number` (1845-1849) :162 | ✅ ALL AGREE |
| **V2 AIO1 serial** | `decode_serial(data, 10, 5)` (IR 1841-1845) :1865 | `aio1_serial_number_new` :76 | `aio1_serial_number` (1841-1845) :166 | ✅ ALL AGREE |
| **V2 AIO2 serial** | `decode_serial(data, 17, 5)` (IR 1848-1852) :1866 | `aio2_serial_number_new` :85 | `aio2_serial_number` (1848-1852) :167 | ✅ ALL AGREE |
| **V2 AIO3 serial** | `decode_serial(data, 24, 5)` (IR 1855-1859) :1867 | `aio3_serial_number_new` :94 | `aio3_serial_number` (1855-1859) :168 | ✅ ALL AGREE |

**GivTCP** has BOTH old and new serial number fields named differently (`aio1_serial_number` vs `aio1_serial_number_new`), while the reference and HEM select based on firmware version. All three ultimately handle both V1 and V2 serial addresses correctly.

---

## EMS HOLDING REGISTERS (HR 2040-2075)

Plant-level EMS configuration at device address 0x11. **HEM does NOT have an EMS decoder** — these registers appear only in the `SAFE_WRITE_REGS` whitelist (registers.rs:499-503). HEM neither reads nor decodes EMS registers.

### HR 2040-2052 — Plant Status + Discharge Slots

| Address | HEM (SAFE_WRITE_REGS) | GivTCP (ems.py) | Reference (ems.py) | Notes |
|---------|----------------------|-----------------|-------------------|-------|
| **HR 2040** | `2040` in SAFE_WRITE_REGS :500 (grouped with discharge slots) | `plant_status` (uint16, Status) :35 | `plant_status` (uint16, Status) :59 | **D2** — HEM mislabels as discharge-related but function is plant master enable. Write-safe inclusion IS correct. |
| HR 2041 | Not in SAFE_WRITE_REGS | `expected_inverter_count` (uint16) :36 | `expected_inverter_count` (uint16) :60 | HEM doesn't expose |
| HR 2042 | Not in SAFE_WRITE_REGS | `expected_meter_count` (uint16) :37 | `expected_meter_count` (uint16) :61 | HEM doesn't expose |
| HR 2043 | Not in SAFE_WRITE_REGS | `expected_car_charger_count` (uint16) :38 | `expected_car_charger_count` (uint16) :62 | HEM doesn't expose |
| HR 2044 | `2044` in SAFE_WRITE_REGS :500 | `discharge_slot_1_start` (uint16, valid 0-2359) :40 | `discharge_slot_1` (timeslot with HR 2045) :63 | ✅ All writable; HEM has correct address |
| HR 2045 | `2045` in SAFE_WRITE_REGS :500 | `discharge_slot_1_end` (uint16) :41 | Part of `discharge_slot_1` timeslot :63 | ✅ All agree |
| HR 2046 | `2046` in SAFE_WRITE_REGS :500 | `discharge_target_1` (uint16, valid 4-100) :42 | `discharge_target_1` (uint16) :65 | ✅ ALL AGREE |
| HR 2047 | `2047` in SAFE_WRITE_REGS :500 | `discharge_slot_2_start` (uint16) :44 | `discharge_slot_2` (timeslot with HR 2048) :66 | ✅ ALL AGREE |
| HR 2048 | `2048` in SAFE_WRITE_REGS :500 | `discharge_slot_2_end` (uint16) :45 | Part of `discharge_slot_2` timeslot :66 | ✅ ALL AGREE |
| HR 2049 | `2049` in SAFE_WRITE_REGS :500 | `discharge_target_2` (uint16, valid 4-100) :46 | `discharge_target_2` (uint16) :67 | ✅ ALL AGREE |
| HR 2050 | `2050` in SAFE_WRITE_REGS :500 | `discharge_slot_3_start` (uint16) :48 | `discharge_slot_3` (timeslot with HR 2051) :68 | ✅ ALL AGREE |
| HR 2051 | `2051` in SAFE_WRITE_REGS :500 | `discharge_slot_3_end` (uint16) :49 | Part of `discharge_slot_3` :68 | ✅ ALL AGREE |
| HR 2052 | `2052` in SAFE_WRITE_REGS :500 | `discharge_target_3` (uint16, valid 4-100) :50 | `discharge_target_3` (uint16) :69 | ✅ ALL AGREE |

### HR 2053-2071 — Charge Slots + Export Slots

| Address | HEM (SAFE_WRITE_REGS) | GivTCP (ems.py) | Reference (ems.py) | Notes |
|---------|----------------------|-----------------|-------------------|-------|
| HR 2053 | `2053` in SAFE_WRITE_REGS :502 | `charge_slot_1_start` (uint16) :52 | `charge_slot_1` (timeslot with HR 2054) :69 | ✅ ALL AGREE |
| HR 2054 | `2054` in SAFE_WRITE_REGS :502 | `charge_slot_1_end` (uint16) :53 | Part of `charge_slot_1` :69 | ✅ ALL AGREE |
| HR 2055 | `2055` in SAFE_WRITE_REGS :502 | `charge_target_1` (uint16, valid 4-100) :54 | `charge_target_1` (uint16) :70 | ✅ ALL AGREE |
| HR 2056-2057 | `2056, 2057` :502 | `charge_slot_2` start/end :55-57 | `charge_slot_2` :71-72 | ✅ ALL AGREE |
| HR 2058 | `2058` :502 | `charge_target_2` (uint16) :58 | `charge_target_2` (uint16) :72 | ✅ ALL AGREE |
| HR 2059-2060 | `2059, 2060` :502 | `charge_slot_3` start/end :59-61 | `charge_slot_3` :73-74 | ✅ ALL AGREE |
| HR 2061 | `2061` :502 | `charge_target_3` (uint16) :62 | `charge_target_3` (uint16) :74 | ✅ ALL AGREE |
| HR 2062-2063 | `2062, 2063` :502 | `export_slot_1` start/end :63-65 | `export_slot_1` :75-76 | ✅ ALL AGREE |
| HR 2064 | `2064` :502 | `export_target_1` (uint16) :66 | `export_target_1` (uint16) :76 | ✅ ALL AGREE |
| HR 2065-2066 | `2065, 2066` :502 | `export_slot_2` start/end :67-69 | `export_slot_2` :77-78 | ✅ ALL AGREE |
| HR 2067 | `2067` :502 | `export_target_2` (uint16) :70 | `export_target_2` (uint16) :78 | ✅ ALL AGREE |
| HR 2068-2069 | `2068, 2069` :502 | `export_slot_3` start/end :71-73 | `export_slot_3` :79-80 | ✅ ALL AGREE |
| HR 2070 | `2070` :502 | `export_target_3` (uint16) :74 | `export_target_3` (uint16) :80 | ✅ ALL AGREE |
| HR 2071 | `2071` :502 | `export_power_limit` (uint16) :75 | `export_power_limit` (uint16) :81 | ✅ ALL AGREE |

### HR 2072-2075 — Additional EMS Controls

| Address | HEM | GivTCP | Reference | Notes |
|---------|-----|--------|-----------|-------|
| HR 2072 | NOT in SAFE_WRITE_REGS | `car_charge_mode` (uint16, valid 0-3) :76 | `car_charge_mode` (uint16) :82 | HEM doesn't expose |
| HR 2073 | NOT in SAFE_WRITE_REGS | `car_charge_boost` (uint16, valid 0-22000) :77 | `car_charge_boost` (uint16) :83 | HEM doesn't expose |
| HR 2074 | NOT in SAFE_WRITE_REGS | `plant_charge_compensation` (uint16, valid -5 to 5) :78 | `plant_charge_compensation` (uint16) :84 | HEM doesn't expose |
| HR 2075 | NOT in SAFE_WRITE_REGS | `plant_discharge_compensation` (uint16, valid -5 to 5) :79 | `plant_discharge_compensation` (uint16) :85 | HEM doesn't expose |

**Note**: HEM's EMS SAFE_WRITE_REGS covers HR 2040-2071 (32 registers). HR 2072-2075 are missing from HEM's whitelist but present in both Python libraries.

---

## CROSS-REFERENCE: Sign Convention Summary

| Register | Wire Convention | HEM Internal | GivTCP | Reference | Agreement |
|----------|----------------|--------------|--------|-----------|-----------|
| Single-phase IR(52) `p_battery` | signed, + = discharge | + = discharge (raw) | + = discharge (raw) | + = discharge (raw) | ✅ ALL AGREE |
| Three-phase IR(1136-1139) | unsigned discharge + unsigned charge | + = discharge (derived: D−C) | N/A (no combined field) | N/A (no combined field) | ✅ Consistent |
| Gateway IR(1702) `p_aio_total` | signed, + = charge | NEGATED → + = discharge | NEGATED → + = discharge | App-level negate | ✅ ALL AGREE |
| Gateway IR(1816-1818) per-AIO | signed, + = charge | NEGATED → + = discharge | NEGATED → + = discharge | App-level negate | ✅ ALL AGREE |
| Single-phase IR(30) `p_grid_out` | signed, + = export | + = export (raw) | + = export (raw) | + = export (raw) | ✅ ALL AGREE |
| Three-phase grid power | derived from unsigned meters | + = export (derived: export−import) | N/A | N/A | ✅ Semantically correct |

---

## GAP ANALYSIS: Registers HEM Does NOT Decode

### Three-phase HR 1000-1079 (82 registers read, NONE decoded)

HEM reads the `THREE_PHASE_HIGH_CONFIG_BLOCK` (80 registers) but `decode_holding_1000_1079` is a no-op. All 80+ register values are discarded. The key ones that could matter:

- HR 1001: `set_command_save` (bool) — three-phase mirror of HR 166
- HR 1005: `REAL_TIME_CONTROL` — three-phase mirror of HR 166
- HR 1063: `p_export_limit` — export limit
- HR 1078: `battery_power_cutoff` — battery power cutoff

### Three-phase HR 1080-1124 (45 registers read, ~20 decoded)

HEM decodes the battery control and schedule registers (1108-1123) but skips:

- HR 1080: `battery_type`
- HR 1088-1107: Various battery/backup configuration parameters
- HR 1117: `load_compensation_enable`
- HR 1124: `battery_maintenance_mode` (**D5**)

### Three-phase IR registers skipped

- IR 1069-1072: `p_inverter_out`, `p_inverter_ac_charge`
- IR 1075-1077: `system_mode`, `status`, `start_delay_time`
- IR 1083-1085, 1091-1096: Per-phase load and output (used only for meter synthesis)
- IR 1120-1121, 1124: `battery_priority`, `battery_type_ir` (**D6**), `dc_status`
- IR 1129-1130, 1133-1135: Temperatures and bus voltages
- IR 1180-1192: EPS (entire block **no-op**)
- IR 1240-1241: `p_export` (alternative export address)
- IR 1300-1307: Fault codes (entire fault bank **not decoded**)
- IR 1317-1319: `tph_software_version`
- IR 1360-1363, 1368-1369, 1372-1373, 1378-1379, 1398-1403: Various daily/total energies

### Gateway IR registers skipped

- IR 1609-1611, 1616, 1619, 1620-1621, 1624-1625: Grid/load voltage/current, fault protection, relay voltages
- IR 1703-1704: AIO state, battery firmware version
- IR 1706-1707, 1709-1710, 1712-1713: Per-AIO charge totals
- IR 1751-1752, 1754-1755, 1757-1758: Per-AIO discharge totals
- IR 1795-1800: Aggregated battery charge/discharge daily/total energy

### EMS registers — entire model not implemented

HEM has NO EMS decoder, blocks, or model. The SAFE_WRITE_REGS whitelist is purely for potential future write support. No EMS data is read or displayed.

---

## APPENDIX: File Reference Table

| Codebase | File | Lines |
|----------|------|-------|
| HEM registers | `givenergy-local/src-tauri/src/modbus/registers.rs` | 1-917 |
| HEM decoder | `givenergy-local/src-tauri/src/inverter/decoder.rs` | 995-1877 (three-phase/gateway sections) |
| GivTCP three-phase | `giv_tcp/GivTCP/givenergy_modbus_async/model/threephase.py` | 1-286 |
| GivTCP gateway | `giv_tcp/GivTCP/givenergy_modbus_async/model/gateway.py` | 1-138 |
| GivTCP EMS | `giv_tcp/GivTCP/givenergy_modbus_async/model/ems.py` | 1-178 |
| GivTCP read.py | `giv_tcp/GivTCP/read.py` | 1547-1712 (gateway path) |
| Ref three-phase | `givenergy-modbus/givenergy_modbus/model/inverter_threephase.py` | 192-441 (_THREE_PHASE_LUT) |
| Ref gateway | `givenergy-modbus/givenergy_modbus/model/gateway.py` | 1-233 |
| Ref EMS | `givenergy-modbus/givenergy_modbus/model/ems.py` | 1-234 |

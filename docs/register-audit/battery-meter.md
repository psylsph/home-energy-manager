# Battery & Meter Register Audit — HEM vs GivTCP vs givenergy-modbus Reference

**Generated:** 2026-06-21  
**Scope:** LV Battery BMS (IR 60-119), HV Battery BCU/BMU (IR 60-119), Meter/CT clamp (IR 60-89)  
**Methodology:** Register-by-register comparison of three codebases; investigation/recording ONLY — no code modifications.

---

## Summary of Key Discrepancies

Each flagged item below cites exact `file:line` evidence. Items marked ⚠️ are confirmed discrepancies; items marked ⚡ are potential issues that warrant review.

### ⚠️ DISCREPANCY 1 — HV BCU battery_power IR(79): Units and sign handling differ

| Source | Converter | Result Units | Sign Handling |
|--------|-----------|-------------|---------------|
| **Reference** | `C.milli` (÷1000) — `hv_bcu.py:39` | kW, **unsigned** (raw uint16 passed to milli) | No int16 conversion — negative values appear as large positive numbers |
| **GivTCP** | `DT.milli` (÷1000) — `hvbcu.py:42` | kW, **unsigned** | Same as reference — no int16 conversion |
| **HEM** | `signed()` (i16 cast, no ÷) — `decoder.rs:1504` | W, **signed** | Correctly handles two's complement |

**Impact:** The reference library and GivTCP treat IR(79) as unsigned, so negative (discharge) power values wrap to >32kW. HEM's `signed()` approach is correct but stores in W while the others store in kW. However, HEM's comment claims "milliwatts" which is wrong — the register is in watts raw, and HEM correctly gets watts by not dividing. **HEM is functionally correct here; reference/GivTCP have a sign bug.**

- HEM: `decoder.rs:1501-1504`
- Reference: `hv_bcu.py:39`
- GivTCP: `hvbcu.py:42`

### ⚡ POTENTIAL ISSUE 2 — GivTCP's `battery_capacity_hv` may not multiply by module count

GivTCP's `register.py:141-143` converter:

```python
def battery_capacity_hv(nom_cap: int) -> Optional[str]:
    return round((nom_cap*76.8)/1000,2)
```

This takes the **per-module** Ah from IR(98) (after `DT.deci` ÷10) and converts to kWh, but does **NOT multiply by `number_of_module`**. Compare with `read.py:438-439` where the display layer does:

```python
bcudata['Stack_Design_Capacity']=round((stack[0].battery_nominal_capacity*stack[0].number_of_module)*0.9,2)
```

So GivTCP's register-level `battery_nominal_capacity` field is per-module kWh (single-module), and the display layer multiplies by module count. This is internally consistent within GivTCP but the field name `battery_nominal_capacity` is misleading (it's per-module, not total pack).

HEM correctly handles this: `decoder.rs:1473-1476` (`HvBcuCluster::total_capacity_ah`) multiplies by `number_of_modules`, and `sanitizer.rs:504` multiplies by nominal voltage.

### ⚡ POTENTIAL ISSUE 3 — HEM lacks absent-battery-slot temperature sentinel rejection

The reference library at `battery.py:37-42` documents that absent battery slots emit `0xF556 = -2730 = -273.0°C` (BMS stores temp with +2730 bias). The reference's temperature bounds `min=-60.0` incidentally reject this sentinel.

HEM's `decode_battery_block` at `decoder.rs:1358` reads `t_max` from IR(103) with no sentinel check:

```rust
let temperature = get_reg(data, 103 - 60) as f32 * 0.1;
```

**If an LV battery slot is empty/unpopulated**, HEM will decode `t_max` as -273.0°C and incorporate it into the temperature average at `sanitizer.rs:490-492`, dragging the display temperature far below any plausible value. The `decode_battery_block` function is only called for detected modules (with valid serial numbers), so this is protected by the serial-based `is_valid()` check. But the HV BMU decode at `decoder.rs:1908-1911` reads per-cell temperatures with no sentinel rejection. HV BMU validation at `decoder.rs:1948-1951` uses serial number, which should filter absent modules. **This is protected by serial validation in practice, but adding a temperature sanity check would be defensive.**

- Reference sentinel documentation: `battery.py:37-42`
- HEM LV decode (no sentinel check): `decoder.rs:1358`
- HEM HV BMU decode (no sentinel check): `decoder.rs:1908-1911`

### ⚠️ DISCREPANCY 4 — HEM meter decode drops IR(66) i_ln

HEM's `decode_meter_data` at `decoder.rs:1276-1300` reads phase currents from IR(63-65) and total current from IR(67), but **skips IR(66) `i_ln`** (neutral-line current). The register map at `registers.rs:530` documents it, but the decoder never reads it.

- HEM skip: `decoder.rs:1288` (reads IR(67)=offset 7, skips offset 6 which is IR(66))
- Reference includes i_ln: `meter.py:36`
- GivTCP includes i_ln: `meter.py:36`

### ⚡ POTENTIAL ISSUE 5 — Meter p_reactive_total and p_apparent_total ignore per-phase

HEM's `MeterData` struct only stores `p_reactive_total` and `p_apparent_total` (no per-phase breakdown), but the decoder `decoder.rs:1293-1294` reads them. The register address comments at `registers.rs:536-543` document the full per-phase layout. The reference and GivTCP expose all per-phase reactive/apparent values. HEM's struct `MeterData` at `model.rs:526-557` has no fields for per-phase reactive/apparent. This is a **data loss** for users who need per-phase power quality data.

- HEM struct: `model.rs:545-548`
- Reference per-phase: `meter.py:42-49`
- GivTCP per-phase: `meter.py:42-49`

---

## A) LV Battery BMS — Input Registers IR 60-119

| IR | Field | HEM (registers.rs + decoder.rs) | GivTCP (battery.py) | Reference (battery.py) | Notes |
|----|-------|------|--------|-----------|-------|
| 60-75 | v_cell_01..16 | `*0.001` mV→V, `decoder.rs:1341-1343` | `DT.milli` (÷1000), `battery.py:29-44` | `C.milli` (÷1000), min=1.0 max=5.0, `battery.py:21-36` | ✅ All agree: mV → V via ÷1000 |
| 76-79 | t_cells groups | `*0.1` deci°C→°C, `decoder.rs:1346-1347` | `DT.deci` (÷10), `battery.py:45-48` | `C.deci` (÷10), min=-60.0 max=150.0, `battery.py:43-46` | ✅ All agree. Reference bounds incidentally reject absent-slot sentinel (-273°C) |
| 80 | v_cells_sum | Not decoded separately (see v_out) | `DT.milli`, `battery.py:49` | `C.milli`, min=16.0 max=80.0, `battery.py:47` | ⚡ HEM skips v_cells_sum; uses v_out (IR 82-83) for terminal voltage instead. Missing diagnostic value. |
| 81 | t_bms_mosfet | Not decoded | `DT.deci`, `battery.py:50` | `C.deci`, min=-60.0 max=150.0, `battery.py:48` | ⚡ HEM skips this. GivTCP uses it as `Battery_Temperature` (read.py:406). |
| 82-83 | v_out | uint32 `*0.001` mV→V, `decoder.rs:1350-1352` | `DT.uint32` + `DT.milli`, `battery.py:51` | `C.uint32` + `C.milli`, min=16.0 max=80.0, `battery.py:49` | ✅ All agree: mV→V |
| 84-85 | cap_calibrated | uint32 `*0.01` → Ah, `decoder.rs:1370,1405` | `DT.uint32` + `DT.centi`, `battery.py:52` | `C.uint32` + `C.centi`, `battery.py:50` | ✅ All agree: centi-Ah → Ah |
| 86-87 | cap_design | uint32 `*0.01` → Ah, `decoder.rs:1371,1406` | `DT.uint32` + `DT.centi`, `battery.py:53` | `C.uint32` + `C.centi`, `battery.py:51` | ✅ All agree |
| 88-89 | cap_remaining | uint32 `*0.01` → Ah, `decoder.rs:1372,1407` | `DT.uint32` + `DT.centi`, `battery.py:54` | `C.uint32` + `C.centi`, `battery.py:52` | ✅ All agree |
| 90-94 | status/warning | Split into 7 status + 2 warning bytes, `decoder.rs:1376-1391` | `duint8` pairs, `battery.py:55-63` | `duint8` pairs, `battery.py:53-61` | ✅ Aligned byte extraction. |
| 95 | — | Unused in all three | — | — | ✅ |
| 96 | num_cycles | uint16 direct, `decoder.rs:1364` | `DT.uint16`, `battery.py:65` | `C.uint16`, `battery.py:63` | ✅ |
| 97 | num_cells | uint16 direct, `decoder.rs:1365` | `DT.uint16`, `battery.py:66` | `C.uint16`, `battery.py:64` | ✅ |
| 98 | bms_firmware_version | uint16 direct, `decoder.rs:1366` | `DT.uint16`, `battery.py:67` | `C.uint16`, `battery.py:65` | ✅ |
| 99 | — | Unused in all three | — | — | ✅ |
| 100 | soc | `u8.min(100)`, `decoder.rs:1355` | `DT.uint16`, `battery.py:69` | `C.uint16`, min=0 max=100, `battery.py:67` | ✅ |
| 101-102 | cap_design2 | **Not decoded by HEM** | `DT.uint32` + `DT.centi`, `battery.py:70` | `C.uint32` + `C.centi`, `battery.py:68` | ⚠️ **HEM missing this register.** HEM uses cap_design (86-87) for `design_capacity_ah`. The reference exposes BOTH cap_design and cap_design2. GivTCP exposes both. The difference between the two on some firmware versions can indicate BMS calibration drift. |
| 103 | t_max | `*0.1` → °C, `decoder.rs:1358` | `DT.deci`, `battery.py:71` | `C.deci`, min=-60.0 max=150.0, `battery.py:69` | ✅ See sentinel note in Discrepancy 3 |
| 104 | t_min | **Not decoded by HEM** | `DT.deci`, `battery.py:72` | `C.deci`, min=-60.0 max=150.0, `battery.py:70` | ⚠️ HEM skips t_min. GivTCP and reference expose it. |
| 105-109 | — | Unused in all three | Reference only: e_battery_discharge_total at IR(105), e_battery_charge_total at IR(106), `battery.py:74-75` | Same as GivTCP | ⚠️ HEM skips IR(105-106) (alternative battery energy totals). GivTCP and reference expose them. These are alternative energy registers that differ from the inverter's IR(36-37). |
| 110-114 | serial_number | 5 regs → Latin-1 chars, `decoder.rs:1361` | `DT.string`, `battery.py:76-78` | `C.serial`, `battery.py:72` | ✅ All three agree: 5 registers = 10 Latin-1 chars |
| 115 | usb_device_inserted | **Not decoded by HEM** | `DT.uint16` + UsbDevice enum, `battery.py:79` | `C.uint16` (raw, since observed values exceed documented enum), `battery.py:76` | ⚡ HEM skips this. Minor — informational only. |
| 116-119 | — | Unused in all three | — | — | ✅ |

### LV Battery — Addressing

| Aspect | HEM | GivTCP | Reference | Notes |
|--------|-----|--------|-----------|-------|
| Battery #1 (primary) | Device 0x32, IR 60-119 via `BATTERY_1_POLL_BLOCK`, `registers.rs:416-421` | Device 0x32 | Device 0x32 | ✅ All agree |
| Additional batteries | Devices 0x33-0x37, `registers.rs:404` | Same range | Same range | ✅ All agree |

### LV Battery — Capacity usage

| Source | Uses cap_design (86-87) or cap_design2 (101-102)? |
|--------|------|
| HEM | `capacity_ah` ← cap_calibrated (84-85). `design_capacity_ah` ← cap_design (86-87). cap_design2 NOT decoded. `decoder.rs:1405-1406` |
| GivTCP | `Battery_Capacity` ← cap_calibrated. `Battery_Design_Capacity` ← cap_design. cap_design2 exposed separately. `read.py:399-401` |
| Reference | Exposes cap_calibrated, cap_design, AND cap_design2. `battery.py:50-51,68` |

---

## B) HV Battery BCU — Input Registers IR 60-119

Device: 0x70-0x8F. BMS aggregation at 0xA0 reports number of BCUs at IR(61).

| IR | Field | HEM (decoder.rs) | GivTCP (hvbcu.py) | Reference (hv_bcu.py) | Notes |
|----|-------|------|--------|-----------|-------|
| 60-63 | pack_software_version | `decode_gateway_version(data, 0)`, `decoder.rs:1492` | `DT.gateway_version`, `hvbcu.py:32-33` | `C.gateway_version`, `hv_bcu.py:30` | ✅ All use gateway_version encoding |
| 64 | number_of_modules | `get_reg(data, 64-60)`, `decoder.rs:1494` | `DT.uint16` → `number_of_module`, `hvbcu.py:34` | `C.uint16`, `hv_bcu.py:31` | ✅ Agree. Note: GivTCP field name uses singular "module". |
| 65 | cells_per_module | `get_reg(data, 65-60)`, `decoder.rs:1495` | `DT.uint16`, `hvbcu.py:35` | `C.uint16`, `hv_bcu.py:32` | ✅ |
| 66 | — | Not decoded (gap) | Not mapped | Not mapped | ✅ |
| 67 | cluster_cell_voltage | `*0.001` mV→V, `decoder.rs:1496` | `DT.uint16` (raw mV, no converter), `hvbcu.py:36` | `C.uint16` (raw mV), `hv_bcu.py:33` | ⚠️ HEM converts to V. GivTCP and reference keep as raw mV. Units differ. |
| 68 | cluster_cell_temperature | `*0.1` deci°C→°C, `decoder.rs:1511` | `DT.uint16` (raw deci°C), `hvbcu.py:37` | `C.uint16` (raw deci°C), `hv_bcu.py:34` | ⚠️ Same as IR(67): HEM converts, GivTCP/reference keep raw. |
| 69 | — | Not decoded | Not mapped | Not mapped | ✅ |
| 70 | status | `get_reg(data, 70-60)`, `decoder.rs:1497` | `DT.uint16`, `hvbcu.py:38` | `C.uint16`, `hv_bcu.py:35` | ✅ |
| 71-72 | — | Not decoded | Not mapped | Not mapped | ✅ |
| 73 | battery_voltage | `*0.1` /10V→V, `decoder.rs:1499` | `DT.deci` (÷10), `hvbcu.py:39` | `C.deci` (÷10), min=0.0 max=1000.0, `hv_bcu.py:36` | ✅ |
| 74 | load_voltage | **Not decoded by HEM** | `DT.deci`, `hvbcu.py:40` | `C.deci`, min=0.0 max=1000.0, `hv_bcu.py:37` | ⚠️ HEM skips. |
| 75 | — | Not decoded | Not mapped | Not mapped | ✅ |
| 76 | battery_current | `signed() * 0.1`, i16 /10A→A, `decoder.rs:1500` | `DT.int16` + `DT.deci`, `hvbcu.py:41` | `C.int16` + `C.deci`, min=-500.0 max=500.0, `hv_bcu.py:38` | ✅ All agree: signed int16 ÷10 → A |
| 77-78 | — | Not decoded | Not mapped | Not mapped | ✅ |
| 79 | battery_power | `signed()` → **W** (int16), `decoder.rs:1504` | `DT.milli` (÷1000) → **kW** (unsigned), `hvbcu.py:42` | `C.milli` (÷1000) → **kW** (unsigned), `hv_bcu.py:39` | ⚠️ **See Discrepancy 1.** HEM uses signed i16 with no division (stores W). Reference/GivTCP use unsigned ÷1000 (stores kW) but lose sign. |
| 80 | battery_soc_max/min | Hi/lo bytes unpacked, `decoder.rs:1506-1508` | `duint8` hi/lo, `hvbcu.py:43-44` | `duint8` hi/lo, min=0 max=100, `hv_bcu.py:40-41` | ✅ |
| 81 | battery_soh | `& 0xFF` as u8, `decoder.rs:1509` | `DT.uint16`, `hvbcu.py:45` | `C.uint16`, min=0 max=100, `hv_bcu.py:42` | ✅ |
| 82-83 | charge_energy_total | **Not decoded by HEM** | `DT.uint32` + `DT.deci` (÷10 kWh), `hvbcu.py:46` | `C.uint32` + `C.deci`, `hv_bcu.py:43` | ⚠️ HEM skips these energy registers. |
| 84-85 | discharge_energy_total | **Not decoded by HEM** | `DT.uint32` + `DT.deci`, `hvbcu.py:47` | `C.uint32` + `C.deci`, `hv_bcu.py:44` | ⚠️ HEM skips. |
| 86-87 | charge_capacity_total | **Not decoded by HEM** | `DT.uint32`, `hvbcu.py:48` | `C.uint32`, `hv_bcu.py:45` | ⚠️ HEM skips. |
| 88-89 | discharge_capacity_total | **Not decoded by HEM** | `DT.uint32`, `hvbcu.py:49` | `C.uint32`, `hv_bcu.py:46` | ⚠️ HEM skips. |
| 90-91 | charge_energy_today | **Not decoded by HEM** | `DT.uint32` + `DT.deci`, `hvbcu.py:50` | `C.uint32` + `C.deci`, `hv_bcu.py:47` | ⚠️ HEM skips. |
| 92-93 | discharge_energy_today | **Not decoded by HEM** | `DT.uint32` + `DT.deci`, `hvbcu.py:51` | `C.uint32` + `C.deci`, `hv_bcu.py:48` | ⚠️ HEM skips. |
| 94-95 | charge_capacity_today | **Not decoded by HEM** | `DT.uint32`, `hvbcu.py:52` | `C.uint32`, `hv_bcu.py:49` | ⚠️ HEM skips. |
| 96-97 | discharge_capacity_today | **Not decoded by HEM** | `DT.uint32`, `hvbcu.py:53` | `C.uint32`, `hv_bcu.py:50` | ⚠️ HEM skips. |
| 98 | battery_nominal_capacity_ah | `*0.1` /10Ah→Ah (per module), `decoder.rs:1513` | `DT.deci` → Ah raw + `DT.battery_capacity_hv` → kWh, `hvbcu.py:54-55` | `C.deci`, `hv_bcu.py:52` | ✅ **All agree it's per-module /10Ah → Ah.** HEM multiplies by module count at `decoder.rs:1474`; GivTCP multiplies at `read.py:438`. |
| 99 | remaining_battery_capacity_ah | `*0.1` /10Ah→Ah (per module), `decoder.rs:1514` | `DT.deci`, `hvbcu.py:56-57` | `C.deci`, `hv_bcu.py:53` | ✅ Same per-module treatment. |
| 100 | number_of_cycles | **Not decoded by HEM** | `DT.deci` (÷10), `hvbcu.py:58` | `C.deci`, `hv_bcu.py:54` | ⚠️ HEM skips. |
| 101 | — | Not decoded | Not mapped | Not mapped | ✅ |
| 102 | min_discharge_voltage | **Not decoded by HEM** | `DT.deci`, `hvbcu.py:59` | `C.deci`, `hv_bcu.py:55` | ⚠️ HEM skips. |
| 103 | max_charge_voltage | **Not decoded by HEM** | `DT.deci` → `min_charge_voltage`, `hvbcu.py:60` | `C.deci`, `hv_bcu.py:56` | ⚠️ HEM skips. Note: GivTCP names this "min_charge_voltage" vs reference "max_charge_voltage". |
| 104 | min_discharge_current | **Not decoded by HEM** | `DT.deci`, `hvbcu.py:61` | `C.deci`, `hv_bcu.py:57` | ⚠️ HEM skips. |
| 105 | max_charge_current | **Not decoded by HEM** | `DT.deci` → `min_charge_current`, `hvbcu.py:62` | `C.deci`, `hv_bcu.py:58` | ⚠️ HEM skips. Note: GivTCP naming discrepancy vs reference. |
| 106-119 | — | Not decoded | Not mapped | Not mapped | ✅ |

### HV Battery — BCU BMS Aggregation (0xA0)

| Aspect | HEM | GivTCP | Reference | Notes |
|--------|-----|--------|-----------|-------|
| BMS aggregation address | `HV_BMS_ADDRESS: 0xA0`, `registers.rs:458` | `slave_address=0xA0`, `client.py:199` | Probes 0xA0 | ✅ All probe 0xA0 |
| BMS register read | IR 60-119, reads IR(61) for num_bcus, `poll.rs:1202-1208` | Reads IR(61) for `number_bcus`, `client.py:202` | Same | ✅ |
| Fallback on BMS failure | Probes BCU 0x70 directly, `poll.rs:1269-1287` | Not in GivTCP client.py (relies on 0xA0) | Not explicitly | ✅ HEM has a more robust fallback |

### HV Battery — BMU (0x50-0x6F)

| Aspect | HEM | GivTCP | Reference | Notes |
|--------|-----|--------|-----------|-------|
| Cell voltage count | 24 cells (HV_CELLS_PER_MODULE), `decoder.rs:1889` | 24 cells, `hvbmu.py:32-55` | 24 cells (_BMU_CELLS), `hv_bcu.py:90` | ✅ All agree on 24 cells |
| Cell voltage scaling | `*0.001` mV→V, `decoder.rs:1905-1906` | `DT.milli` (÷1000), `hvbmu.py:32-55` | `_milli` ÷1000, `hv_bcu.py:131` | ✅ |
| Cell temperature scaling | `*0.1` deci°C→°C, `decoder.rs:1909-1910` | `DT.deci` (÷10), `hvbmu.py:56-79` | `_deci` ÷10, `hv_bcu.py:134` | ✅ |
| Serial number | 5 regs Latin-1 at offset 54-59, `decoder.rs:1913` | 5 regs at offset 54-59, `hvbmu.py:80-82` | 5 regs at offset 54-59, `hv_bcu.py:142` | ✅ |
| Temperature register base | Slice offset 30 (IR 90+base), `decoder.rs:1909` | IR(90+offset*120), `hvbmu.py:56` | IR(90+base), `hv_bcu.py:139` | ✅ Base IR 90 for cell temps |
| Per-module SOC | Backfilled from BCU cluster, `decoder.rs:1964-1984` | Not per-module (stack-level only), `read.py:434-436` | Not per-module | ✅ HEM is more complete |

---

## C) Meter / CT Clamp — Input Registers IR 60-89

Device: 0x01-0x08. All indices below are relative to IR 60 (so offset = IR_address - 60).

| IR | Field | HEM (decoder.rs) | GivTCP (meter.py) | Reference (meter.py) | Notes |
|----|-------|------|--------|-----------|-------|
| 60 | v_phase_1 | `get(0) * 0.1` /10V→V, `decoder.rs:1282` | `DT.deci` (÷10), `meter.py:30` | `C.deci`, min=0.0 max=500.0, `meter.py:30` | ✅ |
| 61 | v_phase_2 | `get(1) * 0.1`, `decoder.rs:1283` | `DT.deci`, `meter.py:31` | `C.deci`, min=0.0 max=500.0, `meter.py:31` | ✅ |
| 62 | v_phase_3 | `get(2) * 0.1`, `decoder.rs:1284` | `DT.deci`, `meter.py:32` | `C.deci`, min=0.0 max=500.0, `meter.py:32` | ✅ |
| 63 | i_phase_1 | `get(3) * 0.01` /100A→A, `decoder.rs:1285` | `DT.centi` (÷100), `meter.py:33` | `C.centi`, `meter.py:33` | ✅ |
| 64 | i_phase_2 | `get(4) * 0.01`, `decoder.rs:1286` | `DT.centi`, `meter.py:34` | `C.centi`, `meter.py:34` | ✅ |
| 65 | i_phase_3 | `get(5) * 0.01`, `decoder.rs:1287` | `DT.centi`, `meter.py:35` | `C.centi`, `meter.py:35` | ✅ |
| 66 | i_ln | **SKIPPED** | `DT.centi`, `meter.py:36` | `C.centi`, `meter.py:36` | ⚠️ **See Discrepancy 4.** HEM skips neutral-line current. |
| 67 | i_total | `get(7) * 0.01`, `decoder.rs:1288` | `DT.centi`, `meter.py:37` | `C.centi`, `meter.py:37` | ✅ (but uses wrong offset name — offset 7 is correct since it's IR 67 = idx 7) |
| 68 | p_active_phase_1 | `signed(8)` int16 W, `decoder.rs:1289` | `DT.int16` W, `meter.py:38` | `C.int16` W, `meter.py:38` | ✅ All treat as signed int16 |
| 69 | p_active_phase_2 | `signed(9)`, `decoder.rs:1290` | `DT.int16`, `meter.py:39` | `C.int16`, `meter.py:39` | ✅ |
| 70 | p_active_phase_3 | `signed(10)`, `decoder.rs:1291` | `DT.int16`, `meter.py:40` | `C.int16`, `meter.py:40` | ✅ |
| 71 | p_active_total | `signed(11)`, `decoder.rs:1292` | `DT.int16`, `meter.py:41` | `C.int16`, `meter.py:41` | ✅ |
| 72 | p_reactive_phase_1 | **Not decoded** | `DT.int16`, `meter.py:42` | `C.int16`, `meter.py:42` | ⚠️ See Discrepancy 5 |
| 73 | p_reactive_phase_2 | **Not decoded** | `DT.int16`, `meter.py:43` | `C.int16`, `meter.py:43` | ⚠️ |
| 74 | p_reactive_phase_3 | **Not decoded** | `DT.int16`, `meter.py:44` | `C.int16`, `meter.py:44` | ⚠️ |
| 75 | p_reactive_total | `signed(15)`, `decoder.rs:1293` | `DT.int16`, `meter.py:45` | `C.int16`, `meter.py:45` | ✅ |
| 76 | p_apparent_phase_1 | **Not decoded** | `DT.int16`, `meter.py:46` | `C.int16`, `meter.py:46` | ⚠️ |
| 77 | p_apparent_phase_2 | **Not decoded** | `DT.int16`, `meter.py:47` | `C.int16`, `meter.py:47` | ⚠️ |
| 78 | p_apparent_phase_3 | **Not decoded** | `DT.int16`, `meter.py:48` | `C.int16`, `meter.py:48` | ⚠️ |
| 79 | p_apparent_total | `signed(19)`, `decoder.rs:1294` | `DT.int16`, `meter.py:49` | `C.int16`, `meter.py:49` | ✅ |
| 80 | pf_phase_1 | **Not decoded** | `DT.milli` (÷1000), `meter.py:50` | `C.milli` (÷1000), `meter.py:50` | ⚠️ |
| 81 | pf_phase_2 | **Not decoded** | `DT.milli`, `meter.py:51` | `C.milli`, `meter.py:51` | ⚠️ |
| 82 | pf_phase_3 | **Not decoded** | `DT.milli`, `meter.py:52` | `C.milli`, `meter.py:52` | ⚠️ |
| 83 | pf_total | `get(23) * 0.001`, `decoder.rs:1295` | `DT.milli`, `meter.py:53` | `C.milli`, `meter.py:53` | ✅ |
| 84 | frequency | `get(24) * 0.01` /100Hz→Hz, `decoder.rs:1296` | `DT.centi` (÷100), `meter.py:54` | `C.centi`, min=40.0 max=70.0, `meter.py:54` | ✅ All agree. |
| 85 | e_import_active | `get(25) * 0.1` /10kWh→kWh, `decoder.rs:1297` | `DT.deci` (÷10), `meter.py:55` | `C.deci`, `meter.py:55` | ✅ |
| 86 | e_import_reactive | **Not decoded** | `DT.deci`, `meter.py:56` | `C.deci`, `meter.py:56` | ⚠️ |
| 87 | e_export_active | `get(27) * 0.1`, `decoder.rs:1298` | `DT.deci`, `meter.py:57` | `C.deci`, `meter.py:57` | ✅ |
| 88 | e_export_reactive | **Not decoded** | `DT.deci`, `meter.py:58` | `C.deci`, `meter.py:58` | ⚠️ |
| 89 | — | Not in decoder | Not mapped | Not mapped | ✅ |

### Meter — Addressing

| Aspect | HEM | GivTCP | Reference | Notes |
|--------|-----|--------|-----------|-------|
| Device addresses | 0x01-0x08, `registers.rs:555` | 0x01-0x08 | 0x01-0x08 | ✅ |
| Poll block | IR 60-89, 30 registers, `registers.rs:558-563` | IR 60-88, 29 registers | IR 60-88, 29 registers | ⚡ HEM reads 1 extra register (IR 89) which doesn't hurt but is unnecessary. |

### Meter — Sign Conventions

The task asks about sign convention for `p_active` registers (IR 68-71). All three codebases use `int16` (signed) with no sign flip. The native Modbus convention for CT meters is typically **positive = import (from grid)** and **negative = export (to grid)**. HEM stores signed values as-is at `decoder.rs:1289-1292`.

HEM's `MeterData` struct at `model.rs:539` documents: "Phase 1-3 active power in W (signed, positive = import)."

GivTCP and reference use the same raw signed int16 convention. **All three agree on sign convention for meter active power.** ✅

---

## Critical Area Deep-Dives

### 1. HV Battery Nominal Capacity: IR(98) — Per-Module Handling

**HEM** (CORRECT ✅):

- `decode_hv_bcu_cluster` decodes IR(98) as `*0.1` → Ah per module (`decoder.rs:1513`)
- `HvBcuCluster::total_capacity_ah()` multiplies by `number_of_modules` (`decoder.rs:1473-1476`)
- `sanitizer.rs:504` computes: `total_capacity_ah() * nominal_v / 1000.0` → kWh
- Nominal voltage for HV models is 76.8V (`model.rs:194`)

**GivTCP** (CORRECT in practice, misleading at register level ✅):

- Register level: `DT.deci` then `battery_capacity_hv(nom_cap)` → `nom_cap * 76.8 / 1000` → per-module kWh (`register.py:141-143`, `hvbcu.py:54`)
- Display level: multiplies by `number_of_module` → total pack kWh (`read.py:438`)
- **The register-level field `battery_nominal_capacity` represents per-module kWh, not total pack.**

**Reference**:

- `C.deci` → Ah per module (`hv_bcu.py:52`). No built-in kWh conversion — leaves to consumers.

**Verdict:** HEM's approach is correct and most explicit. GivTCP is functionally correct but field naming is misleading.

### 2. HV Battery Power: IR(79) — Sign Convention and Units

| Source | Raw interpretation | Final units | Handles negative? |
|--------|-------------------|-------------|-------------------|
| **HEM** | `signed()` u16→i16→i32 | W | ✅ Yes (correct) |
| **GivTCP** | `DT.milli` ÷1000 on uint16 | kW | ❌ No (wraps negatives) |
| **Reference** | `C.milli` ÷1000 on uint16 | kW | ❌ No (wraps negatives) |

HEM's approach is the only correct one. See Discrepancy 1 for details.

For sign convention: All three assume the raw register follows the BMS convention where positive = discharge. HEM and reference/GivTCP don't flip the sign, so they agree on the convention (positive = discharge). ✅

### 3. Absent Battery Sentinel (0xF556 = -273.0°C)

- **Reference**: `battery.py:37-42` documents the sentinel. Temperature bounds `min=-60.0` incidentally reject it.
- **GivTCP**: Same bounds via `DT.deci` (no explicit sentinel rejection but -60.0 floor catches it).
- **HEM**: **No sentinel rejection.** `decoder.rs:1358` (LV) and `decoder.rs:1908-1911` (HV BMU) decode temperatures without bounds checks.
  - **Mitigation in practice**: HEM's `decode_battery_block` is only called for modules that pass serial-based validation (`is_valid`). Similarly, HV BMU reading is guarded by `validate_hv_bmu` which checks the serial. So absent modules should never reach the temperature decoder.
  - **Risk**: If serial validation is bypassed or a module with a valid serial has corrupted temperature registers, garbage temperatures would propagate to the snapshot.

### 4. LV Battery Serial Number: IR(110-114)

All three codebases agree: 5 registers, 10 Latin-1 characters. ✅

- HEM: `decoder.rs:1361` → `decode_serial(data, 110-60, 5)` at `decoder.rs:187-201`
- GivTCP: `battery.py:76-78` → `DT.string, None, IR(110), IR(111), IR(112), IR(113), IR(114)`
- Reference: `battery.py:72` → `C.serial, None, IR(110), IR(111), IR(112), IR(113), IR(114)`

### 5. Cell Voltage Scaling

All three agree: mV → V via ÷1000. ✅

- HEM: `*0.001` at `decoder.rs:1342` (LV) and `decoder.rs:1905-1906` (HV BMU)
- HEM `model.rs:586`: `cell_voltages: Vec<f32>` — stored in V
- GivTCP: `DT.milli` (÷1000)
- Reference: `C.milli` (÷1000)

### 6. Meter p_active Register (IR 68-71) — Sign Convention

All three use signed int16 with no sign flip. Native convention: positive = import from grid, negative = export to grid. ✅

- HEM: `signed(8)` at `decoder.rs:1289`, documented "positive = import" at `model.rs:539`
- GivTCP: `DT.int16` at `meter.py:38-41`
- Reference: `C.int16` at `meter.py:38-41`

### 7. cap_design vs cap_design2

| Source | cap_design (86-87) | cap_design2 (101-102) | Used for capacity? |
|--------|-------------------|----------------------|-------------------|
| **HEM** | ✅ Decoded → `design_capacity_ah` | ❌ Not decoded | cap_calibrated → `capacity_ah` |
| **GivTCP** | ✅ Decoded | ✅ Decoded | cap_calibrated → `Battery_Capacity` |
| **Reference** | ✅ Decoded | ✅ Decoded | Both exposed; consumer decides |

HEM lacks cap_design2. On some firmware versions, cap_design2 holds a more accurate value. **This is a missing register.**

### 8. HV BCU Address 0xA0 for BMS Aggregation

- **HEM**: ✅ Polls 0xA0. Reads IR(61) for number of BCUs. Falls back to direct 0x70 probe on failure. `poll.rs:1202-1287`
- **GivTCP**: ✅ Polls 0xA0. Reads IR(61). Falls back to 1 BCU on failure. `client.py:189-202`
- **Reference**: Probes 0xA0.

All three agree. ✅

---

## Appendix: Complete Field Coverage Summary

### LV Battery Fields — HEM Missing

| Field | Register | Present in Reference? | Present in GivTCP? |
|-------|----------|----------------------|-------------------|
| v_cells_sum | IR(80) | ✅ | ✅ |
| t_bms_mosfet | IR(81) | ✅ | ✅ |
| t_min | IR(104) | ✅ | ✅ |
| cap_design2 | IR(101-102) | ✅ | ✅ |
| e_battery_discharge_total | IR(105) | ✅ | ✅ |
| e_battery_charge_total | IR(106) | ✅ | ✅ |
| usb_device_inserted | IR(115) | ✅ | ✅ |

### HV BCU Fields — HEM Missing

| Field | Register | Present in Reference? | Present in GivTCP? |
|-------|----------|----------------------|-------------------|
| load_voltage | IR(74) | ✅ | ✅ |
| charge_energy_total | IR(82-83) | ✅ | ✅ |
| discharge_energy_total | IR(84-85) | ✅ | ✅ |
| charge_capacity_total | IR(86-87) | ✅ | ✅ |
| discharge_capacity_total | IR(88-89) | ✅ | ✅ |
| charge_energy_today | IR(90-91) | ✅ | ✅ |
| discharge_energy_today | IR(92-93) | ✅ | ✅ |
| charge_capacity_today | IR(94-95) | ✅ | ✅ |
| discharge_capacity_today | IR(96-97) | ✅ | ✅ |
| number_of_cycles | IR(100) | ✅ | ✅ |
| min_discharge_voltage | IR(102) | ✅ | ✅ |
| max_charge_voltage | IR(103) | ✅ | ✅ |
| min_discharge_current | IR(104) | ✅ | ✅ |
| max_charge_current | IR(105) | ✅ | ✅ |

### Meter Fields — HEM Missing

| Field | Register | Present in Reference? | Present in GivTCP? |
|-------|----------|----------------------|-------------------|
| i_ln | IR(66) | ✅ | ✅ |
| p_reactive_phase_1 | IR(72) | ✅ | ✅ |
| p_reactive_phase_2 | IR(73) | ✅ | ✅ |
| p_reactive_phase_3 | IR(74) | ✅ | ✅ |
| p_apparent_phase_1 | IR(76) | ✅ | ✅ |
| p_apparent_phase_2 | IR(77) | ✅ | ✅ |
| p_apparent_phase_3 | IR(78) | ✅ | ✅ |
| pf_phase_1 | IR(80) | ✅ | ✅ |
| pf_phase_2 | IR(81) | ✅ | ✅ |
| pf_phase_3 | IR(82) | ✅ | ✅ |
| e_import_reactive | IR(86) | ✅ | ✅ |
| e_export_reactive | IR(88) | ✅ | ✅ |

---

## Summary Verdict

**HEM is generally well-aligned with the reference and GivTCP for the registers it decodes.** The following actionable findings emerged:

1. **BUG (Reference/GivTCP):** HV BCU `battery_power` IR(79) is treated as unsigned — negative (discharge) values wrap. HEM's `signed()` approach is correct.
2. **MISSING:** HEM should add `cap_design2` (IR 101-102) to the LV BatteryModule decode.
3. **MISSING:** HEM should add per-phase meter power quality fields (reactive, apparent, power factor per phase) and `i_ln`.
4. **MISSING:** HEM should add HV BCU energy registers (charge/discharge totals and today values at IR 82-97).
5. **COSMETIC:** HEM's `decoder.rs:1501` comment incorrectly labels IR(79) as "milliwatts" — it's watts raw.
6. **DEFENSIVE:** Consider adding temperature sanity bounds to catch the absent-battery-slot sentinel even though serial validation currently prevents it.

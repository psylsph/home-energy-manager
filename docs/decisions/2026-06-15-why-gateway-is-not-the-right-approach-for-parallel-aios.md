# Why the Gateway-Direct Approach Is Not the Right Solution for Parallel AIOs

**Date:** 2026-06-15
**Issue:** [#78 — "support for aio's running in parallel with a gateway"](https://github.com/psylsph/home-energy-manager/issues/78)

## Context

GivEnergy supports running up to 3 × AIO (All-in-One) units in parallel through a single Giv-Gateway. When configured this way, users interact with the Gateway rather than the individual AIOs. The issue reporter ("mwardy1972") asked HEM to support this configuration.

## The Obvious (But Wrong) Approach

Connect HEM directly to the Giv-Gateway via Modbus TCP, read the Gateway's register bank (IR 1600–1859), and treat it as the single data source. The Gateway exposes aggregated telemetry: total PV power, total load, per-AIO SoC, combined battery energy, etc.

## Why This Is the Wrong Approach

### 1. Read-Only Dead End — No Control Path

The Gateway's register map (per `givenergy-modbus` `model/gateway.py`) defines **only Input Registers**. There are no documented Holding Registers for the standard inverter controls:

| Control | Gateway Register? |
|---|---|
| Charge power limit (HR111/HR1110) | ❌ Not exposed |
| Discharge power limit (HR112/HR1108) | ❌ Not exposed |
| Battery SOC reserve (HR110/HR1109) | ❌ Not exposed |
| Schedule slots (HR 31-57 / 240-298) | ❌ Not exposed |
| Enable charge/discharge (HR58/HR59) | ❌ Not exposed |
| Mode switching | ❌ Not exposed |

Connecting to the Gateway **sacrifices all control** — HEM becomes a read-only dashboard displaying less-detailed data than the GE Cloud app already provides for free. This defeats the entire purpose of HEM's Control page, schedule programming, and Cosy tariff integration.

### 2. You Lose Per-AIO Detail

The Gateway aggregates data into combined totals. HEM cannot display:

- Per-module battery cell voltages (BMU cell-level data)
- Per-AIO inverter temperatures
- Per-AIO DC solar power (only combined AC-side PV is available)
- Individual inverter diagnostics and fault codes
- Per-AIO firmware versions

These are available by reading each AIO directly at its own Modbus endpoint (port 8899, address 0x11).

### 3. The Single-AIO Case Dominates

The most common installation by far is **1 × AIO + 1 × Gateway** (the Gateway adds whole-home backup). In this scenario:

- Connecting to the Gateway is strictly worse than connecting to the AIO directly
- The AIO exposes a richer register map, full control, and simpler firmware (no V1/V2 gate)
- The Gateway adds complexity (V1 vs V2 firmware byte-order differences for uint32 totals, separate serial number addresses) for zero benefit

Supporting the Gateway as a first-class polling target would force HEM to maintain **two parallel code paths** for what is essentially the same hardware — one for direct AIO and one for Gateway-wrapped AIO. The Gateway path would be strictly inferior.

### 4. Individual AIO Dongles Remain Accessible

When AIOs are in parallel mode, each AIO still has its own network-attached dongle (port 8899). These dongles continue to serve their standard register map on their own slave address (0x11). The community forum confirms that parallel AIO users run GivTCP/Predbat successfully by reading each AIO individually.

**The correct approach is to discover all AIO dongles on the network and connect to each one independently**, not to go through the Gateway.

## What HEM Already Does (Read-Only Gateway Support Exists)

The Gateway read path is already implemented in HEM:

| Component | Status |
|---|---|
| `DeviceType::Gateway` (0x7001-0x70ff) | ✅ Defined in `model.rs` |
| Gateway IR blocks (IR 1600-1859, 5 blocks) | ✅ Defined in `registers.rs` |
| Gateway block reads in poll loop | ✅ Added via `model_specific_blocks_in_poll_order()` in `client.rs` — read after model detection |
| Gateway decoders (all 5 blocks + V1/V2) | ✅ Complete in `decoder.rs` — maps to `per_aio_soc`, `per_aio_power`, `parallel_aio_count`, etc. |
| Gateway-specific snapshot fields | ✅ `parallel_aio_count`, `per_aio_soc`, `gateway_software_version`, etc. in `model.rs` |
| Higher power thresholds for Gateway aggregates | ✅ Sanitizer in `poll.rs` uses 20 kW/25 kW ceilings for Gateway systems |
| BMS/BCU probing skipped for Gateway | ✅ `is_batteryless()` returns true |

This is a **read-only convenience path** — if HEM detects a Gateway (DTC 0x70xx), it can display the aggregated data. But it should **never** be the primary/only data source for the application.

## The Correct Approach

For users with parallel AIOs:

1. **Network discovery** — HEM's existing discovery (`discovery.rs`) already probes the `/24` subnet for GivEnergy dongles on port 8899. Each AIO in a parallel stack will be found by this scan. **No changes needed.**

2. **Connect to each AIO independently** — Each AIO answers at slave address 0x11 on its own port 8899. HEM reads standard register blocks just like any other inverter. **No changes needed.**

3. **Future: Multi-inverter aggregation** — If HEM finds multiple AIOs sharing the same Gateway serial number or DTC family, it could optionally display them as a grouped system in the UI. This is a **UI-only** concern, not a backend protocol change.

4. **The Gateway as a supplemental convenience** — HEM already reads Gateway registers when detected. These provide a useful cross-check (especially `parallel_aio_count` and `per_aio_soc`) but should never replace direct AIO reads for the primary data path.

## Summary

| | Gateway-direct approach | Direct AIO approach |
|---|---|---|
| **Data quality** | Aggregated, less detail | Per-unit, full detail |
| **Control** | ❌ None | ✅ Full (charge/discharge, slots, modes) |
| **Firmware** | V1/V2 complexity | Standard register map |
| **Implementation** | New parallel code path | Already works today |
| **Single-AIO case** | Worse than direct connection | Optimal |

The right answer to "support AIOs running in parallel" is: **HEM already supports it** — each AIO appears as a separate discoverable device on the network. No protocol-level changes are needed.

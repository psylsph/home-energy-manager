# GivEnergy Inverter Simulator — Design Document

## 1. Purpose

A Modbus TCP server that impersonates a real GivEnergy inverter dongle, allowing:

- Full end-to-end testing without physical hardware
- Simulating inverter models you don't own (3-phase, AC-coupled, All-in-One, EMS, etc.)
- Automated integration tests against realistic register data
- Manual QA with the real frontend connecting to a fake inverter

The simulator runs as a separate binary in the same Cargo workspace, sharing the existing protocol types and frame encoding from `src-tauri/src/modbus/`.

---

## 2. Architecture

```
givenergy-local/  (Cargo workspace)
├── src-tauri/         # existing app (library crate)
│   └── src/
│       └── modbus/    # shared: framer, registers, model types
└── simulator/         # NEW: binary crate
    ├── Cargo.toml
    └── src/
        ├── main.rs        # CLI entrypoint, TCP listener
        ├── server.rs      # Connection handler, frame dispatch
        ├── state.rs       # Register bank, model profiles, time simulation
        ├── profiles.rs    # Per-model register defaults + scaling
        └── scenario.rs    # Scenario file loader (JSON/TOML)
```

The simulator depends on `givenergy-local` (the library crate) so it reuses `framer`, `registers`, and `model` types directly. No code duplication.

**Data flow:**

```
Frontend  ──WS──▸  Axum server (port 7337)  ──Modbus──▸  Simulator (port 8899)
                                                         (instead of real inverter)
```

The real app just points at `localhost:8899` instead of `192.168.1.36:8899`. Everything else is identical.

---

## 3. Protocol Implementation

The existing `framer.rs` handles encoding/decoding of the proprietary frame format. The simulator reuses the decoder and adds a response encoder.

### 3.1 Frame format (recap)

```
Bytes 0-1:    Transaction ID      — 0x5959
Bytes 2-3:    Protocol ID         — 0x0001
Bytes 4-5:    Length              — u16 big-endian
Byte  6:      Unit ID             — 0x01
Byte  7:      Function ID         — 0x02 (transparent)
Bytes 8-17:   Data adapter serial — 10 bytes Latin-1
Bytes 18-25:  Padding             — u64 big-endian (value 8)
Byte  26:     Slave address       — 0x11 (writes), 0x32 (reads), 0x32-0x36 (batteries)
Byte  27:     Inner function code — 0x03/0x04/0x06
Bytes 28+:    Inner payload
Last 2 bytes: CRC-16/Modbus over bytes 26+ (big-endian)
```

### 3.2 Response construction

For a **read response** (FC 0x03 or 0x04):

```
Sub-frame payload:
  [serial:10B][padding:8B][slave_addr:1B][func:1B][base_reg:2B][count:2B][data:N*2B][crc:2B]
```

The `serial` in the response is the inverter's dongle serial (configurable in the simulator, e.g. `"CE2052G072"`).

For a **write response** (FC 0x06):

```
Sub-frame payload:
  [serial:10B][padding:8B][slave_addr:1B][func:1B][register:2B][value:2B][crc:2B]
```

For an **exception response**:

```
Sub-frame payload:
  [serial:10B][padding:8B][slave_addr:1B][func|0x80:1B][exception_code:1B][crc:2B]
```

### 3.3 CRC

Reuse the existing `crc16_modbus()` from `framer.rs`. The request CRC covers bytes from the inner function code onward. For responses, compute CRC the same way. (The existing client is lenient about response CRC, so exact matching isn't critical, but we should compute it correctly.)

---

## 4. Register Model

### 4.1 Register banks

The simulator maintains separate register banks per slave address:

```rust
struct SimulatedInverter {
    /// Inverter holding registers (slave 0x11 and 0x32 share these)
    holding: HashMap<u16, u16>,
    /// Inverter input registers (slave 0x32)
    input: HashMap<u16, u16>,
    /// Battery modules: slave_addr 0x32..=0x36 → registers
    batteries: HashMap<u8, BatteryModule>,
    /// The inverter model profile
    profile: InverterProfile,
    /// Configurable serial number (dongle serial, 10 chars)
    dongle_serial: [u8; 10],
}

struct BatteryModule {
    input: HashMap<u16, u16>,  // IR 60-119 for this battery
}
```

Using `HashMap<u16, u16>` rather than a fixed array because different models have different register ranges (single-phase uses IR 0-59, 3-phase uses IR 1000-1413, etc.).

### 4.2 Read handling

When a read request arrives:

1. Parse the frame (reuse existing `FrameDecoder`)
2. Extract slave_address, inner function code (0x03 or 0x04), base_register, count
3. Look up the appropriate register bank
4. Build response frame with register values (missing registers = 0)
5. Encode and send

Special handling for slave addresses:
- `0x11` or `0x32` → inverter holding registers (FC 0x03) or input registers (FC 0x04)
- `0x33..=0x36` → battery module input registers
- Unknown slave → exception response (slave not found)

### 4.3 Write handling

When a write request arrives (FC 0x06):

1. Parse the frame, extract register and value
2. Optionally validate against the safe-write whitelist (can be strict or permissive mode)
3. Update the holding register bank
4. Optionally simulate exception code 67 (dongle busy) with configurable probability
5. Send write response echoing the register + value

Write-triggered side effects (optional, for realism):
- Writing HR(59)=1 (enable_discharge) could start gradually changing battery current
- Writing HR(96)=1 (enable_charge) could start increasing SOC
- Writing HR(163)=100 (reboot) could simulate a brief disconnect

---

## 5. Inverter Model Profiles

### 5.1 Profile structure

```rust
struct InverterProfile {
    /// Device type code (HR 0) — determines model family
    device_type_code: u16,
    /// Model name for display
    name: String,
    /// Nominal battery voltage (for scaling)
    nominal_battery_voltage: f64,
    /// Number of battery modules (0 = no battery support)
    battery_modules: u8,
    /// Supported register ranges
    input_ranges: Vec<(u16, u16)>,    // (start, count) blocks
    holding_ranges: Vec<(u16, u16)>,
    /// Default register values
    default_holding: HashMap<u16, u16>,
    default_input: HashMap<u16, u16>,
}
```

### 5.2 Built-in profiles

Based on the DTC (device type code) classification from giv_TCP:

| Profile | DTC | Battery V | Phases | Notes |
|---------|-----|-----------|--------|-------|
| HybridGen3 | 0x2001 | 51.2V | 1 | The one you own (CE2052G072) |
| ACCoupled | 0x3001 | 51.2V | 1 | AC-coupled storage |
| ThreePhase | 0x4001 | 76.8V | 3 | Different register layout (IR 1000+) |
| AllInOne | 0x8001 | 307V | 1 | HV battery, uses BCU/BMU addresses |
| HybridHV | 0x8101 | 307V | 1 | HV Gen3 |
| EMS | 0x5001 | 51.2V | 1 | Multi-inverter plant controller |
| Gateway | 0x7001 | varies | 1 | Manages AIO units |

### 5.3 Default register values per profile

Each profile populates its `default_holding` and `default_input` maps with realistic values. Examples for a Gen3 Hybrid:

**Holding registers:**
| Register | Value | Meaning |
|----------|-------|---------|
| HR(0) | 0x2001 | Device type code |
| HR(13-17) | serial bytes | Inverter serial |
| HR(20) | 0 | enable_charge_target = false |
| HR(21) | 395 | ARM firmware version |
| HR(27) | 1 | battery_power_mode = eco |
| HR(55) | 100 | battery capacity = 100 Ah |
| HR(59) | 0 | enable_discharge = false |
| HR(94) | 700 | charge slot 1 start = 07:00 |
| HR(95) | 1000 | charge slot 1 end = 10:00 |
| HR(96) | 0 | enable_charge = false |
| HR(110) | 4 | battery SOC reserve = 4% |
| HR(111) | 50 | charge limit = 50% |
| HR(112) | 50 | discharge limit = 50% |
| HR(116) | 100 | charge target SOC = 100% |

**Input registers:**
| Register | Value | Meaning |
|----------|-------|---------|
| IR(0) | 1 | inverter status = normal |
| IR(1) | 0 | PV1 voltage = 0V (night) |
| IR(2) | 0 | PV2 voltage = 0V |
| IR(5) | 2410 | grid voltage = 241.0V |
| IR(13) | 5000 | grid frequency = 50.00Hz |
| IR(30) | 50 | grid power = 50W importing |
| IR(50) | 5120 | battery voltage = 51.20V |
| IR(51) | 0 | battery current = 0A |
| IR(52) | 0 | battery power = 0W |
| IR(56) | 250 | battery temperature = 25.0°C |
| IR(59) | 75 | battery SOC = 75% |

### 5.4 3-Phase differences

The 3-phase profile shifts data to different register ranges:
- PV data: IR 1000-1060 (per-string, separate registers)
- Grid data: IR 1060-1120 (3-phase voltages, currents, power per phase)
- Battery: IR 1120-1140
- Energy totals: IR 1360-1413 (all uint32 pairs)
- Charge/discharge slots: HR 1113-1122

The profile's `input_ranges` field tells the simulator which register ranges are valid reads. Requests outside these ranges return all-zeros (matching real inverter behaviour).

---

## 6. Battery Simulation

### 6.1 LV Battery modules (slave 0x32-0x36)

Each battery module has its own register bank (IR 60-119):

| Register | Value | Meaning |
|----------|-------|---------|
| IR(60-75) | 3200-3400 | Cell voltages in mV (16 cells) |
| IR(76-79) | 200-350 | Cell group temperatures ×0.1°C (4 groups) |
| IR(82-83) | 51200 | Total voltage in mV (uint32) |
| IR(84-85) | 10000 | Capacity (uint32, 0.01 Ah = 100Ah) |
| IR(96) | 250 | Cycle count |
| IR(97) | 16 | Cell count |
| IR(100) | 75 | SOC % |
| IR(103) | 300 | Temperature max ×0.1°C |
| IR(104) | 200 | Temperature min ×0.1°C |
| IR(110-114) | serial bytes | Battery serial |

### 6.2 HV Battery (All-in-One, HV Gen3)

HV models use BCU/BMU hierarchy at different slave addresses:
- BAMS: slave 0xA0 (reports BCU count)
- BCU: slave 0x70+ (reports module count, SOC, SOH)
- BMU: slave 0x50+ (24 cell voltages + 24 cell temps per module)

Supporting HV batteries requires extending the slave address dispatch but follows the same pattern. This can be a later phase — start with LV batteries since the developer owns one.

---

## 7. Time-Based Simulation

### 7.1 Continuous state evolution

The simulator runs a background tick (configurable interval, default 1s) that evolves register values over time:

```rust
struct SimState {
    /// Current simulation time (accelerated or real)
    sim_time: Instant,
    /// Solar irradiance curve (0-1000 W/m² over a day cycle)
    solar_curve: fn(time_of_day: f64) -> f64,
    /// Base load pattern (W, varies by time of day)
    load_curve: fn(time_of_day: f64) -> f64,
    /// Current SOC (evolves based on charge/discharge)
    soc: f64,
    /// Time acceleration factor (1.0 = real time, 3600.0 = 1 hour per second)
    time_scale: f64,
}
```

### 7.2 Tick logic

Every tick:

1. Advance simulation clock by `delta * time_scale`
2. Calculate solar power from irradiance curve × panel capacity
3. Calculate home load from load curve
4. Determine battery charge/discharge based on mode and solar surplus
5. Update SOC: `soc += (battery_power * delta_hours) / (capacity_ah * nominal_voltage) * 100`
6. Update input registers with new values:
   - IR(1-2): PV voltages (proportional to irradiance)
   - IR(8-9): PV currents
   - IR(18-20): PV power
   - IR(30): grid power (positive = exporting)
   - IR(50-52): battery voltage/current/power
   - IR(59): battery SOC (rounded)
   - IR(41): inverter temperature (slowly drifts based on power throughput)

### 7.3 Solar curve example

A simple sinusoidal model for a UK summer day:

```
Irradiance(t) = max(0, 800 * sin(π * (t - 6) / 12))  for 6:00 ≤ t ≤ 18:00
                0                                              otherwise
```

With 5kW of panels: `pv_power = irradiance / 1000 * 5000`

### 7.4 Load curve example

A simple stepped model:

```
Load(t) = 300W   (00:00-06:00, overnight baseload)
          800W   (06:00-09:00, morning peak)
          400W   (09:00-15:00, daytime)
          1200W  (15:00-20:00, evening peak)
          500W   (20:00-00:00, evening)
```

---

## 8. Scenario Configuration

### 8.1 TOML format

Scenario files define a starting state and optional time evolution:

```toml
[profile]
model = "HybridGen3"         # or "ThreePhase", "AllInOne", etc.
serial = "SIM2024G001"       # 10-char dongle serial
dongle_serial = "SIM2024G001"

[battery]
count = 1                    # number of LV battery modules
capacity_ah = 100
initial_soc = 75

[simulation]
time_scale = 3600.0          # 1 hour per second (full day in 24s)
tick_interval_ms = 1000

# Override specific registers
[overrides.holding]
59 = 1                       # enable_discharge = true
27 = 1                       # eco mode

[overrides.input]
5 = 2410                     # grid voltage 241.0V
59 = 75                      # SOC 75%
```

### 8.2 Preset scenarios

Built-in presets for common test cases:

| Preset | Description |
|--------|-------------|
| `sunny-day` | Full solar generation day cycle with eco mode |
| `night-drain` | No solar, battery discharging through evening |
| `charging` | Grid charging with enable_charge=1 |
| `idle` | Everything zero, inverter waiting |
| `fault` | Inverter status = fault, fault codes set |
| `3ph-sunny` | 3-phase model with balanced generation |
| `aio-hv` | All-in-One with HV battery stack |
| `no-battery` | PV-only (DTC 0x23xx), no battery registers |
| `stress` | Maximum values on all registers |

### 8.3 CLI interface

```bash
# Run with default profile (Gen3 Hybrid, idle)
cargo run -p givenergy-simulator

# Run with a specific preset
cargo run -p givenergy-simulator -- --preset sunny-day

# Run with a scenario file
cargo run -p givenergy-simulator -- --scenario scenarios/3ph-charging.toml

# Override serial and port
cargo run -p givenergy-simulator -- --serial CE2052G072 --port 9999

# List available profiles and presets
cargo run -p givenergy-simulator -- --list

# Run without time simulation (static registers, responds to writes)
cargo run -p givenergy-simulator -- --static

# Simulate dongle busy on register 32 with 50% probability
cargo run -p givenergy-simulator -- --fault-register 32 --fault-probability 0.5
```

---

## 9. Integration with Tests

### 9.1 Rust integration tests

The simulator can be started programmatically in test fixtures:

```rust
// In src-tauri/tests/integration_test.rs

use givenergy_simulator::Simulator;

#[tokio::test]
async fn test_read_input_registers() {
    let sim = Simulator::builder()
        .profile("HybridGen3")
        .serial("TEST0001G0")
        .battery(1, 100, 75)  // 1 module, 100Ah, 75% SOC
        .listen_port(0)       // OS assigns free port
        .start()
        .await
        .unwrap();

    let addr = sim.local_addr(); // get assigned port

    // Connect with the real ModbusClient
    let mut client = ModbusClient::connect(addr).await.unwrap();
    let regs = client.read_input_registers(0x32, 0, 60).await.unwrap();

    assert_eq!(regs[0], 1);           // status = normal
    assert_eq!(regs[59], 75);         // SOC = 75%

    sim.shutdown().await;
}
```

### 9.2 End-to-end frontend tests

Start the simulator + the Axum server together, then test via HTTP/WebSocket:

```rust
#[tokio::test]
async fn test_frontend_gets_snapshot() {
    let sim = Simulator::builder()
        .preset("sunny-day")
        .start()
        .await
        .unwrap();

    let app = spawn_app(sim.local_addr()).await; // start Axum with sim as inverter

    // Wait for first poll
    tokio::time::sleep(Duration::from_secs(2)).await;

    let resp = reqwest::get(format!("http://{}/api/status", app.addr))
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let snapshot: InverterSnapshot = resp.json().await.unwrap();
    assert!(snapshot.solar_power > 0);

    sim.shutdown().await;
}
```

### 9.3 Existing unit tests

The existing 86 tests in `src-tauri/` don't need changes — they test decoder/encoder logic with raw register data. The simulator adds a new layer: integration tests that exercise the full Modbus client → simulator path.

---

## 10. Implementation Phases

### Phase 1: Static simulator (MVP)

- TCP listener on configurable port
- Frame parsing (reuse `framer.rs`)
- Read register banks for inverter + batteries
- Write register handling (update bank, respond)
- Gen3 Hybrid profile only
- Single CLI binary with `--serial`, `--port`, `--preset` flags
- No time simulation — registers stay static unless written

**Effort:** ~2-3 days
**Value:** Immediate — can test all read/write paths without hardware

### Phase 2: Multiple profiles + scenarios

- Add profiles: AC-coupled, 3-phase, All-in-One, PV-only
- Scenario file support (TOML)
- Preset scenarios
- 3-phase register layout (IR 1000+)
- HV battery (BCU/BMU slave addresses)

**Effort:** ~2-3 days
**Value:** Can test models you don't own, verify 3-phase decode logic

### Phase 3: Time simulation

- Background tick loop
- Solar/load curves
- SOC evolution
- Time acceleration
- Configurable via scenario files

**Effort:** ~2 days
**Value:** Realistic demo data, visual testing of charts and energy flow

### Phase 4: Test integration

- `Simulator::builder()` API for programmatic use in tests
- Integration test examples in `src-tauri/tests/`
- CI workflow that starts simulator + Axum + runs API tests
- Simulator binary published for manual QA

**Effort:** ~1-2 days
**Value:** Automated regression testing, CI confidence

---

## 11. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Separate binary or feature flag? | Separate binary crate | Cleaner separation, no feature-flag pollution in the main app |
| Shared code? | Yes, depend on `givenergy-local` lib | Reuse framer, registers, model types |
| Register storage | `HashMap<u16, u16>` | Flexible for different model register ranges |
| Config format | TOML for scenarios | Rust-native, human-readable, good for nested config |
| Async runtime | tokio (matching existing app) | Consistent, enables reuse of async code |
| CRC validation | Strict on request, compute on response | Matches real inverter behaviour |

---

## 12. File Structure

```
simulator/
├── Cargo.toml
└── src/
    ├── main.rs          # CLI parser, start TCP server
    ├── server.rs        # TcpListener, connection loop, frame dispatch
    ├── state.rs         # SimulatedInverter, register banks, read/write logic
    ├── profiles.rs      # Built-in model profiles with default register values
    ├── scenario.rs      # TOML scenario parser
    ├── simulation.rs    # Time-based state evolution (Phase 3)
    └── battery.rs       # Battery module simulation
```

Estimated total: ~1500-2000 lines of Rust across all phases.

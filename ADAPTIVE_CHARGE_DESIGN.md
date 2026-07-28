# Adaptive Charge Mode Design

## Objective

Add an **Adaptive Charge** option to the existing Charging Mode selector. It
automatically changes the inverter's maximum battery charge rate according to
time of day, battery state of charge (SOC), and user-configured hysteresis.

Adaptive Charge is mutually exclusive with Cosy and every Agile mode. Discharge
protection remains inverter-native through the existing per-slot Target SOC and
a new global Discharge Cutoff SOC control using HR114 where supported.

Adaptive Charge changes only the maximum permitted charge rate. It does not
force grid charging and cannot guarantee that solar power is available.

## Charging modes

The selector contains:

- Standard
- Cosy
- Agile — Full
- Agile — Charge only
- Agile — Discharge only
- Adaptive Charge

Only one mode may be selected. Selecting Adaptive Charge disables Cosy and
Agile, captures the current manual charge limit, and starts the Adaptive Charge
state machine. Leaving Adaptive Charge restores the captured limit.

A single backend mode-transition endpoint should own the complete transition.
This replaces parallel frontend requests that could otherwise leave multiple
modes partly enabled after one request fails.

## User interface

### Adaptive Charge periods

Adaptive Charge supports up to four periods. Each period contains:

| Setting | Purpose |
| --- | --- |
| Enabled | Include or exclude the period |
| All day | Apply for the full day |
| Start / End | Active local-time window, including overnight windows |
| Preferred charge rate | Normal maximum charge rate |
| Low SOC threshold | SOC at which recovery charging starts |
| Recovery SOC | SOC at which recovery charging ends |
| Recovery charge rate | Higher maximum charge rate while SOC is low |

Rates use the same normalized 0–100% display and estimated kW value as the
existing charge-rate control.

Example:

- Period: 08:00–17:00
- Preferred rate: 42% (approximately 2 kW on the reporter's system)
- Low SOC: 30%
- Recovery SOC: 40%
- Recovery rate: 100% (approximately 3.6 kW)

The controller normally caps charging at the preferred rate. At or below 30%
SOC it permits the recovery rate until SOC reaches 40%. Outside the period it
restores the previous manual limit.

### Validation

- Low SOC is 4–99%.
- Recovery SOC is greater than Low SOC and no more than 100%.
- Preferred and recovery rates must be supported by the connected device.
- Recovery rate must be at least the preferred rate.
- Enabled periods must not overlap.
- A non-all-day period must have different start and end times.
- Overnight periods such as 22:00–06:00 are valid.
- At least one period must be enabled before Adaptive Charge is activated.

### Status

The UI shows one runtime state:

- Outside configured period
- Preferred rate active
- Low-SOC recovery active
- Suspended by Auto Winter
- Restoring manual rate
- Waiting for inverter
- Error

It also shows the current period, desired rate, and estimated kW.

While Adaptive Charge is active, the manual charge-rate control is disabled and
explains that Adaptive Charge owns the setting. The direct charge-rate API
returns HTTP 409 while Adaptive Charge owns the register.

## Configuration model

Conceptual Rust structures:

```rust
enum ChargingMode {
    Standard,
    Cosy,
    AgileFull,
    AgileCharge,
    AgileDischarge,
    Adaptive,
}

struct AdaptiveChargeConfig {
    periods: Vec<AdaptiveChargePeriod>,
    confirmation_readings: u32,
}

struct AdaptiveChargePeriod {
    enabled: bool,
    all_day: bool,
    start_hour: u8,
    start_minute: u8,
    end_hour: u8,
    end_minute: u8,
    low_soc: u8,
    recovery_soc: u8,
    preferred_rate_percent: u8,
    recovery_rate_percent: u8,
}
```

Rates are persisted as normalized user-facing percentages. The backend converts
them to raw model-specific register values.

Runtime state:

```rust
enum AdaptiveChargeState {
    Inactive,
    OutsideWindow,
    Preferred { period: usize },
    Recovery { period: usize },
    SuspendedAutoWinter,
    Restoring,
    Error { message: String },
}

struct SavedChargeLimit {
    inverter_serial: String,
    device_type_code: String,
    register_address: u16,
    raw_value: u16,
}
```

The raw value is retained so restoration is exact.

## State-machine behaviour

The controller evaluates valid, sanitized inverter snapshots.

### Outside an active period

The desired limit is the saved manual charge limit. Adaptive Charge remains
selected but does not impose a configured rate outside its periods.

### Entering a period

- SOC at or below Low SOC enters Recovery.
- Any higher SOC enters Preferred.

### Preferred

Apply the preferred rate. Two consecutive snapshots at or below Low SOC move
the controller to Recovery.

### Recovery

Apply the recovery rate. Remain in Recovery throughout the hysteresis band. Two
consecutive snapshots at or above Recovery SOC return to Preferred.

Time-boundary changes apply immediately. Periods use local system time and are
half-open: start inclusive and end exclusive.

A register write occurs only when the desired raw limit differs from inverter
readback and the same value is not already pending. Failed writes remain
unconfirmed and are retried on a later poll. External changes are reasserted on
the following poll while Adaptive Charge is active.

## Auto Winter priority

Auto Winter temporarily overrides Adaptive Charge.

When Auto Winter activates, Adaptive Charge enters `SuspendedAutoWinter`,
restores the saved manual limit once, and stops reasserting adaptive values.
When Auto Winter ends, the current period and SOC are evaluated and Adaptive
Charge resumes.

## Discharge protection

### Per-slot Target SOC

The Gen3 Discharge Schedule's existing Target SOC remains the preferred
slot-specific protection. A discharge slot configured with a 25% target should
stop scheduled discharge at 25%.

### Global Discharge Cutoff SOC

Add a **Discharge Cutoff SOC** control beside Minimum SOC:

- Minimum SOC controls the Eco/self-consumption reserve.
- Discharge Cutoff SOC is the hard battery floor intended to apply in timed
  modes as well.

For verified single-phase devices this writes HR114 through the existing
`SetPowerReserve` command. The backend decodes HR114 into a nullable unified
snapshot field, validates 4–100%, exposes a control endpoint, and capability
gates the UI.

Three-phase HR1078 has conflicting descriptions in reference code and is not
treated as equivalent until its behaviour is confirmed on hardware.

The cutoff is an inverter setting and is not restored when Adaptive Charge is
disabled. Expected inverter behaviour is to stop at the floor and resume if
charging raises SOC above it.

## Backend API

### Charging mode

```text
GET  /api/charging-mode
POST /api/charging-mode
```

Example request:

```json
{"mode":"adaptive"}
```

Enabling Adaptive Charge requires a connected inverter, a valid snapshot, a
readable current charge-rate value, and valid configuration.

### Adaptive configuration

```text
GET  /api/adaptive-charge
POST /api/adaptive-charge
```

The response includes configuration and runtime status. Saving configuration
wakes the poll loop for immediate re-evaluation.

### Discharge cutoff

```text
POST /api/control/discharge-cutoff
```

Example request:

```json
{"soc":25}
```

Unsupported devices return a clear error.

Existing Cosy and Agile APIs remain compatible but route mode changes through
the central transition logic.

## Device-aware conversion

| Device family | Register | Conversion |
| --- | ---: | --- |
| DC hybrid, including HY3.6 G3 | HR111 | Normalized percentage ÷ 2, rounded to 0–50 |
| AC-coupled | HR313 | Direct 1–100 |
| Three-phase/HV | HR1110 | Direct 1–100 |

The backend is the source of truth for conversion. The UI displays estimated kW
using existing battery-capacity and model-power calculations.

## Persistence and crash recovery

Persist the selected charging mode, Adaptive periods, saved pre-Adaptive raw
limit, inverter identity, and whether restoration is pending.

On restart while Adaptive Charge remains selected, reconnect to the same
inverter and continue without overwriting the original baseline. If restoration
was interrupted, retain the baseline until readback confirms it.

Never restore a saved value to a different inverter. Enter an error state and
require the user to reset or re-enable the mode.

If the application closes during an active period, the inverter retains the
last written rate. The UI warns that the app must remain running. Graceful
shutdown restoration is not relied upon.

## Settings migration

Add a persisted `charging_mode` field. For older files:

1. `cosy_enabled = true` maps to Cosy.
2. A non-Off `agile_scope` maps to the corresponding Agile mode.
3. Otherwise use Standard.

Keep legacy fields synchronized temporarily for API compatibility. Invalid
legacy files with both Cosy and Agile enabled use current precedence and log a
warning.

## Observability

Add snapshot fields for charging mode, Adaptive state, active period, desired
rate, and suspension reason. Log transitions and writes, not unchanged poll
evaluations.

## Test plan

### Rust

- Configuration validation and settings migration
- All-day, daytime, overnight, and boundary period selection
- Overlap rejection
- Preferred and Recovery transitions with hysteresis and confirmation
- Period changes and outside-window restoration
- Duplicate-write suppression and retry behaviour
- External override correction
- Auto Winter suspension and resumption
- Crash recovery and inverter-identity protection
- Device-specific normalized-rate conversion
- HR114 decode, validation, and route selection
- Charging-mode mutual exclusion and manual-rate conflict response

### Frontend

- Adaptive option and configuration visibility
- Period editing and validation
- Rate/kW estimates
- Disabled manual control while Adaptive owns it
- Runtime and Auto Winter status
- Discharge Cutoff capability gating

### Integration

Use the simulator to verify HR111 transitions, period entry/exit, baseline
restoration, restart recovery, and HR114 write/readback. The simulator does not
currently enforce HR114 in battery physics, so physical cutoff behaviour must
be confirmed on a real Gen3 inverter before release.

## Acceptance scenario for issue #234

Given an HY3.6 G3 with an 08:00–17:00 Adaptive period, approximately 2 kW
preferred rate, 30% Low SOC, 40% Recovery SOC, approximately 3.6 kW Recovery
rate, and a 25% discharge target/cutoff:

1. Charging is normally capped near 2 kW during the period.
2. Clouds may still cause discharge because the rate is a ceiling, not forced
   charging.
3. At 30% SOC the permitted rate rises near 3.6 kW.
4. Recovery remains active until SOC reaches 40%.
5. Scheduled discharge cannot take the battery below 25%.
6. At 17:00 the original manual charge limit is restored.
7. Auto Winter temporarily suspends Adaptive Charge.
8. Cosy and Agile cannot run concurrently with Adaptive Charge.

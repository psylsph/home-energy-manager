# Issue #316: Adaptive Charge stalls at a zero charge limit

## Status and scope

Diagnosed, then implemented: `observed_charge_rate_is_valid()` in
`src-tauri/src/inverter/state_machines.rs` now accepts **0–50 inclusive**
for `HR_BATTERY_CHARGE_LIMIT`, matching the encoder's write contract, while
direct-percentage registers keep the stricter 1–100 range. The regression
coverage listed below accompanies the fix. No Timed Export ownership or
scheduling changes were made.

Issue: <https://github.com/psylsph/home-energy-manager/issues/316>

The report concerns Windows 11 running v0.83.2, with Timed Export active and solar generation clipping on 15 and 16 September 2026. The reporter disabled Adaptive Charge and manually increased the Battery Charge Power Limit, after which solar generation increased.

The investigation examined the issue, its screenshot and attached log, and the source at tag `v0.83.2`. All line references below refer to that tag, not the current working branch.

**Confidence:** high for the zero-limit blockage on 16 September. The evidence does not conclusively explain the entire 15 September report, whose afternoon log contains successful Adaptive Charge writes. No hardware reproduction or tests were run during the investigation.

## Evidence

Log attachment: <https://github.com/user-attachments/files/32296176/Filter.log20260916.txt>

The following events occur in the second day's portion of the log, after the midnight rollover:

| Time | Evidence |
| --- | --- |
| 06:48:38–06:48:39 | `SetChargeLimit` encodes a write to register 111 with value 0; the write succeeds. |
| 08:24:45–11:20:08 | Adaptive Charge emits 545 warnings rejecting `observed_raw=0` for `Gen3Hybrid`. |
| 11:20:30 onwards | Charge-limit commands increase the raw limit through 6, 8, 14, 17 and 22, consistent with the reporter's manual intervention. |
| 11:52:25 | Adaptive Charge successfully writes register 111 = 16 in `preferred` state. |
| 11:53:37 | Adaptive Charge successfully writes register 111 = 22 in `restoring` state. |

The repeated warning is:

```text
Adaptive Charge: ignoring invalid observed charge limit device_type=Gen3Hybrid observed_raw=0
```

These are raw register values, not normalized UI percentages. On this model, a raw value of 50 corresponds to 100%.

The attachment also shows successful Adaptive Charge writes while Timed Export is active. Timed Export is therefore not, by itself, the condition blocking Adaptive Charge here. The log proves that HEM issued the zero-limit command, but does not establish which person or API client requested it.

## Root cause

The normal charge control and Adaptive Charge disagree about the valid range of the same register:

| Code at v0.83.2 | Behaviour |
| --- | --- |
| `src-tauri/src/inverter/encoder.rs:402–406` | `SetChargeLimit` accepts raw values **0–50**. |
| `src-tauri/src/inverter/decoder.rs:1094` | Reads register 111 into `snapshot.charge_rate`. |
| `src-tauri/src/inverter/state_machines.rs:727–732` | `observed_charge_rate_is_valid()` accepts only **1–50** for that register. |
| `src-tauri/src/inverter/state_machines.rs:836–849` | An observed zero logs the warning and returns without a write or desired rate. |
| `src-tauri/src/inverter/poll.rs:3186–3243` | Calls the state machine and only writes a charge limit when its outcome contains a write. |

The causal chain is:

1. HEM successfully sets a legitimate zero charge limit.
2. Subsequent snapshots report that zero limit.
3. Adaptive Charge treats the value as invalid on every poll.
4. It returns before evaluating the configured period and SOC recovery logic.
5. It cannot raise the limit, leaving the inverter at zero until another control changes it.

This explains why Adaptive Charge did not intervene on 16 September and is consistent with the reported clipping and improvement after the manual increase. The screenshot alone does not establish every physical cause of clipping.

When no baseline has been saved, this branch also sets the adaptive state to `Inactive`. When a baseline already exists, it leaves the previous state unchanged, despite taking no action.

## Related failure paths

### Adaptive Charge can reject its own output

`AdaptiveChargeConfig::validate()` accepts rates from 0–100% (`src-tauri/src/settings/mod.rs:918–925`). The single-phase conversion maps 0% to raw zero (`state_machines.rs:710–715`).

A configured 0% preferred rate can therefore be written successfully, then rejected on the following poll. The early return prevents the SOC confirmation counters from advancing, so Adaptive Charge cannot switch to a higher recovery rate when SOC falls.

### Zero baselines cannot be restored

The same validity helper validates saved baselines, including during disable (`state_machines.rs:798–805`). A zero baseline is rejected rather than restored. The correction must cover observed values and saved baselines consistently.

## Proposed fix

In `src-tauri/src/inverter/state_machines.rs`, align Adaptive Charge's raw-value validation with the model-specific write contract:

- Accept **0–50 inclusive** for `HR_BATTERY_CHARGE_LIMIT`.
- Retain **1–100 inclusive** for the supported direct-percentage registers whose write contracts require that range.
- Continue rejecting unsupported devices and out-of-range values.
- Keep the existing stable-baseline capture requirement and inverter-identity checks.
- Do not change Timed Export ownership or scheduling to address this issue.

The expected minimal production change is to `observed_charge_rate_is_valid()`. Its callers should then permit zero both as an observed single-phase limit and as a saved baseline. Review all callers and verify the full lifecycle rather than changing only the initial-enable path.

Zero is a legitimate setting, so the numeric value alone cannot distinguish it from a corrupt zero read. Preserve existing read-validity protections; any further corruption detection should use evidence about the read or snapshot, not a blanket rejection of a valid register value.

## Regression coverage

Add or update inline Rust tests in `src-tauri/src/inverter/state_machines.rs`:

1. **Enable from zero:** two stable, valid Gen3 snapshots with raw limit zero capture a zero baseline and allow the configured nonzero rate to be written.
2. **Recover from a zero preferred rate:** start from a nonzero baseline, apply a 0% preferred rate, feed its zero readback, then feed the required low-SOC confirmations. Assert that the recovery-rate write occurs.
3. **Restore zero on disable:** capture zero, apply a nonzero adaptive rate, disable Adaptive Charge, and assert a zero restoration write. Clear the saved baseline only after matching readback.
4. **Restore zero outside the period:** leaving an adaptive-owned period restores the zero baseline and reaches `OutsideWindow` after readback.
5. **Handle zero with an existing baseline:** an observed zero must not bypass period evaluation or SOC recovery merely because ownership was already established.
6. **Preserve model-specific bounds:** test zero and both range boundaries for the single-phase register, and retain zero rejection for direct-percentage registers requiring 1–100.
7. **Preserve safety checks:** retain coverage for unstable baseline readings, genuinely out-of-range values, unsupported devices, and saved baselines belonging to a different inverter.

The existing test `adaptive_baseline_ignores_invalid_rates_and_requires_stability()` at `state_machines.rs:3642` currently treats both 0 and 100 as invalid Gen3 readings. It encodes the faulty assumption. Replace zero in its invalid-value cases with a genuinely invalid value and add explicit valid-zero lifecycle tests; retain the stability assertions.

Use deterministic snapshots and explicit times. Any tests involving settings persistence or backend state must use isolated temporary configuration directories, never live settings.

## Verification and remaining uncertainty

For implementation, first demonstrate the zero-limit failures with regression tests, then apply the minimal fix and run the focused tests followed by the required project checks, including `cargo test`, `cargo clippy`, and `npm run test`.

A hardware follow-up should verify that enabling Adaptive Charge from a zero limit produces the configured rate and that a 0% preferred rate can transition to recovery. This was not performed during diagnosis.

The 15 September portion needs additional context before attributing every reported symptom to this bug: Adaptive Charge successfully wrote preferred limits of 16 and 8 that afternoon and later restored 5. The configured periods, rates, SOC thresholds and contemporaneous SOC would help distinguish a separate issue from expected configured behaviour.

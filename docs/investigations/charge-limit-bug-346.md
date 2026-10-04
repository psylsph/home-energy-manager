# Issue #346: the charge power limit did not limit the charge power

## Status and scope

Fixed in two steps. The first fix (v0.85.1) changed the figure shown beside the
slider and left the value written to the inverter untouched, so the battery
kept charging at full power. A follow-up report showed that, and this document
records the corrected diagnosis and fix.

Issue: <https://github.com/psylsph/home-energy-manager/issues/346>

No hardware was available to us. The register semantics below come from the
GivTCP reference implementation, which has run against real inverters, and are
confirmed against the reporter's measured charge power. Where something is
inference rather than measurement it is marked **inferred**.

## What was reported

A Gen 1 Hybrid user set the Battery Charge Power Limit to 66% and the readout
still said "2.6 kW", with the History page showing a 2.6 kW charge. After the
v0.85.1 label change they set 62%, the Control page showed "1.6 kW", and the
battery still charged at about 2.4 kW overnight.

## The register is a percentage of battery capacity

The single-phase DC-hybrid charge and discharge limits (`HR_BATTERY_CHARGE_LIMIT`
and `HR_BATTERY_DISCHARGE_LIMIT`, HR 111/112, 0-50) are not a percentage of the
inverter's maximum power. GivTCP, the only reference validated on hardware,
stores and reads them as a percentage of battery capacity, where 50 means 0.5C:

- `GivTCP/write.py:541` stores `min(watts / (capacity_wh / 2) * 50, 50)`.
- `GivTCP/read.py:661` reads back `min(register / 100 * capacity_wh, inverter_max_w)`.

`givenergy-modbus` documents the same register only as "0-50%", which is why the
first investigation read it as a percentage of the maximum.

For the reporter (about 9.5 kWh nominal, 2.6 kW inverter) the old mapping of
62% to register 31 asks for 31% of 9500 Wh = 2945 W. That is above what the
inverter can deliver, so the inverter's own ceiling applied: it limited nothing.
The same arithmetic explains the original report, where the capacity-based label
that HEM showed before v0.85.1 ("2.6 kW") was the correct reading of register 33.

## Why the first fix did not work

The v0.85.1 change replaced the capacity-based label with `percent / 100 * max`.
The write path was not touched: 62% still sent register 31 before and after, so
the inverter received identical instructions and could not behave differently.
The new label merely agreed with HEM's forecast, which held the same wrong
assumption. The investigation noted the register's semantics as unverified under
"Known limitations" and shipped regardless.

## Why the tests did not catch it

Every test checked that the code agreed with itself:

- the label tests asserted `percentToWatts(66, 2600) === 1716`, the formula that
  had just been written;
- the routing tests checked the register the API wrote was the one the decoder
  read;
- the forecast test checked the forecast and the UI used the same rule;
- the page tests pinned `66% -> 33` on the wire, the value the old code already
  sent.

The end-to-end tests stopped at "the right register was written", and the
GivEnergy Simulator held the same wrong belief: it applied the limit as a
percentage of the device maximum, so even a test measuring charge power would
have seen 1.6 kW. Both sides of every test agreed, and none checked the
assumption against an independent source.

## The fix

One conversion, used everywhere a "percent of the inverter maximum" meets the
half-scale register: `src/lib/powerLimit.ts` and
`src-tauri/src/inverter/power_limit.rs`.

- **Write:** `register = round(percent / 100 * max_w / capacity_w * 100)`, with
  100% writing 50 (the register maximum and factory default, "no limit"), so the
  top of the slider can never be throttled by a misread capacity.
- **Read:** `percent = min(100, round(register * capacity_w / max_w / 100 * 100))`.
  Anything at or above the register that reaches the inverter's maximum, including
  the factory default 50, reads as 100%.
- The maximum used is capped at `capacity / 2`, because register 50 is 0.5C
  however the maximum is stated. On a pack where `capacity / 2` is the inverter's
  rating the mapping degenerates to the old "double the register".
- When the maximum or the capacity is unknown there is nothing to scale by and
  the register falls back to the plain doubling.
- The direct registers (AC-coupled and Gateway HR 313/314, three-phase and HV
  HR 1110/1108) are percentages of the inverter's maximum and are unchanged.

Consumers moved onto the shared conversion: the Control page sliders and their
save path, the Inverter page readout, Adaptive Charge's preferred and recovery
rates, the forecast's battery rate limits, and the forecast plan's charge-slot
apply path. The sliders now move in 1% steps, and the label shows the percentage
the inverter will actually hold, because neighbouring percentages share a
register value (about 4% apart on a 9.5 kWh pack behind a 2.6 kW inverter).

Review of the first version of this fix tightened the conversion further:

- **A non-zero percentage never writes register 0.** On a 9.5 kWh pack 1% is
  register 0.27, and writing 0 stops charging while the label claims a limit. The
  smallest real limit is register 1; only a deliberate 0% writes 0.
- **An implausible capacity is treated as unknown.** HR 55 has no sanitiser on
  single-phase inverters, so one corrupt read can report thousands of kWh and
  collapse the ratio. Outside 1-150 kWh the conversion falls back to the plain
  doubling rather than acting on it.
- **A partial charge rate is refused when the pack size is unknown.** The
  forecast plan's charge-slot request needs the pack size for anything below
  100%, and silently halving is the mapping that limits nothing on a large pack.
  100% (register 50) never needs it.
- **The Telegram mode message** converts the register the same way as the UI
  instead of printing it as a percentage.
- **One scale object.** `PowerLimitScale::from_snapshot` carries the bank, the
  maximum and the capacity to every conversion, and
  `tests/fixtures/power-limit-vectors.json` is asserted by both the Rust and the
  TypeScript implementation so they cannot drift apart.

The three secondary fixes from the first pass stand: the Gateway uses the AC
limit bank on every path, the Inverter Active Power Limit is hidden where HEM
cannot write it, and AC three-phase polls the block its Active Power Limit
needs.

## Simulator

`givenergy-simulator` commit `f116623` models the single-phase DC-hybrid limit as
a percentage of capacity (and leaves the direct banks as percentages of the
maximum). The old behaviour is why the HEM end-to-end suite could not see this.

## Tests

The expected values come from the GivTCP formulas, not from HEM's own
arithmetic.

- `tests/lib/powerLimit.test.ts` and `src-tauri/src/inverter/power_limit.rs`
  check the conversion against GivTCP's read and write formulas for the
  reporter's pack, a small pack, unknown inputs, an overstated maximum, 100% and
  the direct banks.
- `tests/pages/controlPagePowerLimitLabel.test.tsx` renders the Control page for
  every device family and asserts both what is shown for a register and what is
  sent for a slider position.
- Rust tests cover the forecast rate limits, Adaptive Charge's conversion and
  state transitions, and the charge-slot apply path.
- `e2e/local-charge-limit-power.spec.ts` sets the limit through the UI against
  the simulator and measures the real charge power. It fails against the
  percent-of-maximum mapping (full-power charging at a 60% limit) and passes with
  the fix. `e2e/local-adaptive-charge.spec.ts` asserts the Adaptive Charge
  register through the same pack-size conversion.

## Known limitations

- **Not yet confirmed on hardware.** The reporter's overnight 2.4 kW is
  consistent with the capacity-relative reading but does not prove it, since 2.4
  kW is also near the inverter's own ceiling. A discriminating check: set the
  limit to about 20% (register 10 on a 9.5 kWh pack) and expect roughly 0.95 kW.
  The percent-of-maximum reading would give about 0.5 kW.
- **A limit saved by v0.85.0 to v0.85.3 keeps its old register.** A value such as
  31 now reads as 100% on a large pack, which is what it did, and has to be set
  again to take effect.
- **Manual limit writes wait behind an active Force Charge.** The write pump
  defers lower-priority writers while Force Charge owns the inverter, so a limit
  changed mid-charge applies once the charge ends. This predates the fix and is
  unrelated to the register semantics. The Control page now holds the "Applying
  changes to inverter" notice until the inverter reads the value back and, after
  20 seconds without it, says the change has not been confirmed and mentions a
  running Force Charge (previously the notice vanished after the POST returned,
  a few milliseconds, which is what the reporter saw as an orange box too quick to
  read).
- **Capacity is read from the inverter.** The conversion follows
  `battery_capacity_kwh`, which the decoder derives from the pack's reported
  amp-hours. Whether that is the total across several batteries on a Gen 1 has
  not been verified against a multi-battery capture.

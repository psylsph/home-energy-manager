# Roadmap

Planned and under-investigation items for Home Energy Manager. This is not a
release commitment; items may change as hardware access, simulator support, and
user reports improve.

Items already shipped have been moved to the changelog. What remains is
either tied to an open user issue or scoped but not yet implemented.

---

## Energy Tariff editor — supplier-shaped (#132, #131)

**Status**: Implementation not started.

Two open issues describe the same root cause: the tariff editor is shaped
around a contiguous 24-hour tiling (`start` of the first slot is locked to
00:00, `end` of the last slot is locked to 23:59, slots must not overlap or
leave gaps), which doesn't match how suppliers publish tariffs.

A real UK tariff is normally a **base rate + a small number of override
windows**. Octopus Flux is "day rate all day, except 02:00–05:00 cheap and
16:00–19:00 peak" — three lines of supplier copy, but five tariff slots in
the current editor. Economy 7 / night-rate tariffs that cross midnight have
to be split into two slots because `end > start` is rejected.

### Proposed change

Switch the editor to a **base rate + override windows** model:

- `base_rate` (p/kWh) applies to anything not covered by a window
- `windows: [{ label, start, end, rate }]` — start/end free, may cross
  midnight, may not overlap
- `rate_for_minutes(t)` currently falls back to the **last slot's rate**
  on a gap (`settings/mod.rs:167`), which is a defensive hack rather
  than a real default. The cleanest rewrite is: (a) add `base_rate` to
  `TariffConfig`, (b) change the engine to return `base_rate` on a
  miss, (c) drop the tiling requirement from `validate()`

### Open questions

1. **Backward compatibility.** Existing `settings.json` has the tiling
   shape. Either convert on load (base rate = first slot's rate, windows
   = remaining slots) or keep the old shape and only expose the new one
   behind a flag. The migration story needs deciding before the editor
   rewrite.
2. **Standing Charge (#131).** A daily p/day field on the import side,
   added into History cost totals as `standing_charge × days_in_range`.
   Cleanest to ship together with the editor rewrite since both touch the
   same model.

### Reference

- Current data model: `src-tauri/src/settings/mod.rs` (`TariffSlot`,
  `TariffConfig`, `rate_for_minutes`, `validate`)
- Current editor: `src/pages/SettingsPage.tsx` (`TariffSlotEditor`)
- Validation: `src/lib/tariff.ts` (`validateTariffConfig`,
  `isTariffConfigValid`)
- Issues: #132 (supplier-shaped tariffs), #131 (Standing Charge)

---

## Visuals & quick-look enhancements

### PV1 / PV2 output as percentage of max (#110)

**Status**: Open issue, not started.

Add a `xx% of max` sub-label next to each string's kW reading on the Solar
page and Status radial diagram, mirroring the V/A sub-label that was added
to the radial solar node in v0.42.0.

### Status page quick-look visuals (#113)

**Status**: Open issue, design TBD.

User wants the Status page to communicate more at a glance. The v0.42.0
refresh already added colour-coded flow lines and battery-tier colours,
but a larger rework (state-of-charge trajectory, today's running totals,
key alert status) may be wanted. Needs a focused design pass before
implementation.

---

## GivCloud DNS re-configuration (dongle-level)

**Status**: Under investigation. Related to
[giv_tcp issue #546](https://github.com/britkat1980/giv_tcp/issues/546).

If GivEnergy Ltd were to go under, the `givenergy.cloud` domain could be
sold to an untrusted third party, potentially giving them control over all
customer installations — even those using local control.

#### Discovery

The GivEnergy dongle exposes a plaintext configuration protocol on
**telnet port 23**. Using `netcat`:

```
$ nc 192.168.X.XXX 23
Login as:admin
Password:admin
CMD>cfg
CFG>prof show
#PROFILE
#VER_2_1
...
M2M_NET2_ENABLE=1
M2M_NET2_PORT=7654
M2M_NET2_SERADD=comms.givenergy.cloud
M2M_NET2_TCPTO=300
...
```

The `M2M_NET2_*` settings control the dongle's connection to the
GivEnergy cloud.

#### Interaction commands

| Command | Description |
|---|---|
| `up` | Navigate up the menu hierarchy |
| `cfg > set M2M_NET2_SERADD <addr>` | Change the cloud server address |
| `cfg > prof save` | Persist changes to flash storage |
| `reboot` | Reboot dongle to apply config changes |

#### Safety notes

- Do **not** just set `M2M_NET2_ENABLE` to `0` — on boot, the inverter
  re-enables it
- Recommended sandbox address: `127.0.0.1` (prevents any outbound cloud
  traffic)
- The dongle emits a lot of debug junk; response traffic needs filtering

#### Proposed implementation

1. **Backend**: Add a lightweight telnet/CLI client to interact with the
   dongle's config shell — parse `prof show` output to extract current
   M2M settings, send `set` commands to override, and trigger
   `prof save` + `reboot`
2. **API**: `GET /api/dongle/cloud-config` (read current M2M settings),
   `POST /api/dongle/cloud-config` (update server address)
3. **Frontend**: Settings page section — show current dongle cloud
   address with an override input (default `127.0.0.1`) and a "Reboot
   dongle" button
4. **Advanced**: For users running a local GivTCP server, allow setting
   the address to their own server (enables fully local-cloud
   alternatives like Axle or Predbat)

---

## Read-only EMS support

**Status**: Partial. EMS / Gateway plant registers (HR 2040–2075) are
polled and decoded because Gateway shares the bank. Standalone EMS
device handling — model detection, dedicated snapshot fields, UI — is
not done.

Known information:

- EMS uses device address `0x11`
- EMS config block: holding registers `2040..2075`
- EMS runtime block: input registers `2040..2094`
- EMS model prefixes: `5` / `51`

Treat as separate work from normal inverter polling. Should ship
read-only until real hardware or simulator coverage is available.

---

## GitHub Actions Node runtime update

GitHub Actions currently reports a non-fatal Node 20 deprecation warning
for some marketplace actions. Update affected actions or opt in to Node
24 when the actions used by the workflow support it cleanly.

---

## Recently shipped (see CHANGELOG for details)

These items previously lived in the roadmap and are now done:

- **Gateway (DTC 0x70xx)** — full IR 1600–1859 aggregation decoders,
  EMS plant registers, model-aware Quick Actions, em-dash fallback for
  unavailable telemetry. Shipped v0.26.0 and refined through v0.40.x.
- **Multi-window time-of-use tariffs** — generic N-slot tariff model
  covering Flux, Cosy, Eco7 and similar. Shipped v0.39.0. Note: uses
  the simpler slot-list model rather than the `MultiZoneTariffConfig`
  redesign sketched in earlier revisions of this file — the simpler
  model covered the same user needs without a breaking change.
- **Power page + CSV export** — `PowerPage.tsx` with history fetch,
  transformed sample model, `calculatePowerReport()`, CSV export of
  metadata / buckets / detailed samples.
- **Cache-Control headers on static assets** — long-cache for
  `/assets/`, no-store for `index.html`. Shipped v0.25.2.
- **AC Three-Phase (DTC 0x60xx)** — full polling, decoder, encoder,
  schedule, write routing.

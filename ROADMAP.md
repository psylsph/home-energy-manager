# Roadmap

Planned and under-investigation items for Home Energy Manager. This is not a
release commitment; items may change as hardware access, simulator support, and
user reports improve.

## Near-term candidates

### GivEnergy EV Charger (EVC) tab

**Status**: Investigation complete; implementation not started.

Add a self-contained EVC page for local monitoring and basic control of a
GivEnergy EV charger.

The EVC should be implemented as a separate feature path, not as part of the
existing inverter polling loop. The inverter data adapter uses GivEnergy's
proprietary transparent Modbus framing on port `8899`; the EVC uses standard
Modbus TCP on port `502` and normally has its own IP address.

#### Proposed scope

- New frontend page: `src/pages/EvcPage.tsx`
- New route/navigation entry: `/evc`
- New backend module: `src-tauri/src/evc/`
- New REST endpoints under `/api/evc/...`
- EVC-specific settings stored in the existing settings file
- No changes to inverter snapshot, history storage, or inverter WebSocket stream

#### Suggested first release

- Read-only charger status
- Manual host/IP setting and enable/disable toggle
- Basic controls:
  - start charging
  - stop charging
  - set charge current limit
  - enable/disable Plug and Go
- Clear warning when the charger cannot be reached or local control is not
  enabled

#### Later options

- EVC discovery on port `502`
- Sync charger clock
- Local Grid/Solar/Hybrid charging modes
- Import-current cap using inverter grid-current data
- Maximum session-energy cap
- Optional history charts for EVC session energy and power

#### Known EVC notes

From the GivTCP reference implementation:

- Local control must be enabled in the GivEnergy portal.
- Older EVC firmware reportedly exposes Modbus only over Wi-Fi, not Ethernet.
- GivTCP reports this as fixed in later firmware; users on older firmware may
  need a firmware update and local-control enablement.
- GivTCP's Grid/Solar/Hybrid EVC modes are locally mimicked behaviours, not
  cloud-synchronised official charger modes.

#### Reference material

- `givenergy-modbus` architecture note:
  - `/home/stuart/repos/givenergy-modbus/docs/architecture.md`
  - Upstream: <https://github.com/dewet22/givenergy-modbus/blob/c81780b21b7f6ff5f8604604130ee80bd009ef83/docs/architecture.md>
- GivTCP EVC implementation:
  - `/home/stuart/repos/giv_tcp/GivTCP/evc.py`
  - Upstream: <https://github.com/GivEnergy/giv_tcp/blob/master/GivTCP/evc.py>
- GivTCP EVC discovery:
  - `/home/stuart/repos/giv_tcp/GivTCP/findEVC.py`
  - Upstream: <https://github.com/GivEnergy/giv_tcp/blob/master/GivTCP/findEVC.py>
- GivTCP EVC user notes:
  - `/home/stuart/repos/giv_tcp/README.md`
  - Upstream: <https://github.com/GivEnergy/giv_tcp#givenergy-electric-vehicle-charger-givevc>

#### Registers found so far

GivTCP reads holding registers `0..59` and `60..114` from the EVC over standard
Modbus TCP.

| Register | Meaning | Scale / values |
|---:|---|---|
| 0 | Charging state | `0=Unknown`, `1=Idle`, `2=Connected`, `3=Starting`, `4=Charging`, `5=Startup Failure`, `6=End of Charging`, `7=System Failure`, `8=Scheduled`, `9=Updating`, `10=Unstable CP` |
| 2 | Connection status | `0=Not Connected`, `1=Connected` |
| 4 | Error code | see GivTCP `EVCLut.error_codes` |
| 6 | Current L1 | `/10` A |
| 8 | Current L2 | `/10` A |
| 10 | Current L3 | `/10` A |
| 13 | Active power | W |
| 17 | Active power L1 | W |
| 20 | Active power L2 | W |
| 24 | Active power L3 | W |
| 29 | Meter energy | `/10` kWh |
| 32 | EVSE max current | A |
| 34 | EVSE min current | A |
| 36 | Charge limit | `/10` A |
| 38-68 | Serial number | ASCII characters, zero skipped |
| 72 | Charge session energy | `/10` kWh |
| 74-76 | Charge start time | hour/minute/second |
| 79 | Charge session duration | seconds |
| 82-84 | Charge end time | hour/minute/second |
| 93 | Plug and Go | `0=enable`, `1=disable` |
| 94 | Charge control display | `0=Ready`, `1=Start`, `2=Stop` |
| 97-102 | Charger system time | year/month/day/hour/minute/second |
| 109 | Voltage L1 | `/10` V |
| 111 | Voltage L2 | `/10` V |
| 113 | Voltage L3 | `/10` V |

#### Controls found so far

| Control | Register | Value |
|---|---:|---|
| Set Plug and Go | 93 | `0=enable`, `1=disable` |
| Set charge current limit | 91 | amps × 10 |
| Start/stop charging | 95 | `0=Ready`, `1=Start`, `2=Stop` |
| Set charger clock | 97-102 | year/month/day/hour/minute/second |

Implementation should validate current-limit writes against the charger-reported
minimum and maximum current before sending register `91`.

## Later candidates

### Read-only EMS support

EMS support should be treated separately from normal inverter polling. Initial
support should be read-only until real hardware or simulator coverage is
available.

Known information from previous investigation:

- EMS uses device address `0x11`
- EMS config block: holding registers `2040..2075`
- EMS runtime block: input registers `2040..2094`
- EMS model prefixes: `5` / `51`

### GitHub Actions Node runtime update

GitHub Actions currently reports a non-fatal Node 20 deprecation warning for
some marketplace actions. Update affected actions or opt in to Node 24 when the
actions used by the workflow support it cleanly.

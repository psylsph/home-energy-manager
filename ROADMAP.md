# Roadmap

Priority order for the open GitHub issues, as of 12 November 2026 (v1.16.0). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs four things: whether users are affected today, whether a later item depends on it, how much it costs, and whether it is waiting on someone else's answer. Cheap items that unblock a user come first, and large or risky items come after the ones that are ready to build. Where it differs from the issue's own label, the reason is given.

## Tier 0: shipped, waiting on real use

Closed since the last pass: #404 (the replan, v1.14.0), #429 (the Today balance bars), #432, #455 (Energy Tariffs from Octopus), #457 (Home without the car), #431 (John confirmed on 12 November that the overnight smart charge held the AIO behind his Gateway; his logs are on the issue), #434 (the smart-charge top-up, v1.13.0; it has not yet seen a real daytime dispatch on Stuart's 0x3001, which should record its ARM firmware; plan in `docs/issue-434-dispatch-charge-plan.md`; not built: Octopus's monthly car-allowance limit), #433 (the battery pause on the models GivTCP gives it, finished in v1.16.0; the AC three-phase keeps its Timed Discharge as a recorded difference), #452 (the Open Dongle Page button: Jet-bundle reports no drop-outs since, so silence counts as success), and #392 and #398, which came off the list. #460 (the What's New link to the changelog, v1.15.0) is built and shipped; it is Stuart's own issue and can be closed. Left over from #455, none of it blocking: an automatic fill of missing History rates after the first Octopus rates sync (today it is a button), the Telegram daily report still prices at the current tariff, and nothing yet reads the new gas rates. #404 is closed but hasn't yet seen a real night of moved slots; if one goes wrong, reopen it (plan in `docs/issue-404-replan-plan.md`). Shipped but still open, waiting on a confirmation (the reporters were nudged on 11 November):

- [453](https://github.com/psylsph/home-energy-manager/issues/453), planning a charge before every cheap period (v1.16.0): an opt-in Plan Every Cheap Window toggle beside the strategy selector, shown on tariffs with more than one cheap period. Each charge is sized to last until the next cheap period, the refresh, auto-apply, plan history and replan run once per period, and touching slots at one price (within 0.05p) count as one period. Waiting on peterb99 to try it on Cosy. Kept trade-off: a period HEM wakes into more than 10 minutes late isn't planned, and slot 1 keeps its old times until the next. Plan in `docs/issue-453-every-cheap-window-plan.md`.
- [458](https://github.com/psylsph/home-energy-manager/issues/458), the EV label saying Charging at 0 W (fixed in v1.14.1: the label says Charging only while power flows, whatever the state register says). Waiting on Brian to update and confirm. His bridge reports state, cable and power from different sources, worth making consistent on his side. Kept trade-off: a car that pauses mid-session reads Connected / Cable In.
- [446](https://github.com/psylsph/home-energy-manager/issues/446) (v1.8.4), waiting on Brian to try the next Saving Session and confirm the export stops where it says and Minimum SOC comes back afterwards. The floor that reaches the 23:30 cheap start is #405.
- [406](https://github.com/psylsph/home-energy-manager/issues/406), dispatch slots as known load (v1.6.0 and v1.7.0). The last comment there (8 November) still says #434 isn't built; it shipped in v1.13.0, so Brian should be told (the top-up is opt-in on the Octopus page under the hold).

Items that have shipped are left off the tables below; Stuart closes the issues once the reporter confirms.

## Tier 1: bugs and user-facing gaps

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [456](https://github.com/psylsph/home-energy-manager/issues/456) | Read house figures from the Gateway while controlling the All-in-One directly | L | The Gateway keeps its own copy of the schedule and battery registers (#428, closed), so HEM points at the AIO to control the battery, but the AIO can't see the house: its grid and load readings describe its link to the Gateway, so Home comes out 0 or negative-derived and the day totals are impossible (John's AIO claimed 16 kWh exported while discharging 1 kWh). The right figures are in the Gateway's input registers, which HEM already decodes when pointed at one, so the fix is a second read-only poll and one merged snapshot with each field from the device that measures it. It touches every page that shows Home, the history rows, the daily report and the planner's consumption profile, so it needs the simulator's Gateway + AIO pairing and John's raw snapshots to settle which registers mean what. Large, and waiting on those readings. |

## Tier 2: protecting users and widening reach

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [397](https://github.com/psylsph/home-energy-manager/issues/397) | Detect a dongle factory reset, add a security checklist | M | Labelled low priority, but with GivEnergy in administration no firmware fix is coming. A reset wipes the schedule silently, and the checklist half is cheap, passive documentation. It must not probe the dongle's other ports. The checklist can have its own Settings section. |
| 2 | [396](https://github.com/psylsph/home-energy-manager/issues/396) | MQTT output with Home Assistant auto-discovery | L | Probably the largest group of potential users, and it gives them one poller on the dongle instead of two. Publish only at first. Control over MQTT later, through the same safety rules as the authenticated control API. The main server's bind and CORS stay as they are. |

## Tier 3: Planner export advice

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. The original post is still ambiguous; confirm it with the reporter before building. |
| 2 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The riskiest item here. The simulation it needs already exists as the Planner's export advice (`plan_export_window`), which projects hourly solar and consumption to the next cheap import window — but it sizes a duration, and a Saving Session's window is Octopus's, so this needs the inverse: the floor that leaves the house covered until 23:30. #446 (shipped in v1.8.4) built the thing it writes to (a held Minimum SOC during a window), #407 is the back-stop for a thin projection; it also builds on the dispatch-as-load handling from #406 and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |

## Tier 4: later

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one: it gives the EV charger a place in the house story, and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |
| 2 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | Moved to the bottom on purpose. Only one person has asked for it, and it changes how tariffs are stored and validated, which feeds History costs, the planner and the tariff automations, so a mistake could break existing installs. If it is picked up, keep it additive: existing saved tariffs must load and price exactly as before, new options such as a base rate with override windows or windows that cross midnight must be opt-in, and the old and new forms need tests that compare costs side by side. Revisit if more people ask. |

## Dependencies at a glance

```text
#428 Gateway keeps its own schedule copy (shipped) ─► #456 house figures from the Gateway while controlling the AIO
#446 held floor during a window (shipped in v1.8.4) ─► #405 calculated floor writes through it
#407 back-stop ─► #405 export surplus
#406 dispatch as known load (shipped) ─► #405 export surplus, which can't export while the car charges
```

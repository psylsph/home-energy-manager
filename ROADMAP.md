# Roadmap

Priority order for the open GitHub issues, as of 11 November 2026 (v1.14.1). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs four things: whether users are affected today, whether a later item depends on it, how much it costs, and whether it is waiting on someone else's answer. Cheap items that unblock a user come first, and large or risky items come after the ones that are ready to build. Where it differs from the issue's own label, the reason is given.

## Tier 0: shipped, waiting on real use

Closed since the last pass: #404 (the replan, v1.14.0), #429 (the Today balance bars), #432, #455 (Energy Tariffs from Octopus), #457 (Home without the car), and #392 and #398, which came off the list. Left over from #455, none of it blocking: an automatic fill of missing History rates after the first Octopus rates sync (today it is a button), the Telegram daily report still prices at the current tariff, and nothing yet reads the new gas rates. #404 is closed but hasn't yet seen a real night of moved slots; if one goes wrong, reopen it (plan in `docs/issue-404-replan-plan.md`). Shipped but still open, waiting on a confirmation (the reporters were nudged on 11 November):

- [434](https://github.com/psylsph/home-energy-manager/issues/434), the battery top-up during Intelligent Octopus Go smart charges (v1.13.0). An opt-in under the dispatch hold: while a fresh *smart* dispatch runs (never a boost) HEM writes charge slot 1 over the dispatch window plus Force Charge, so the inverter stops by itself at the end even with HEM away, then puts back the user's own settings. It yields to every other charge owner, and the Planner and History price each smart half-hour at the off-peak rate. Not built: Octopus's monthly car-allowance limit. Waiting on the first real daytime dispatch on Stuart's 0x3001, recording its ARM firmware. Plan in `docs/issue-434-dispatch-charge-plan.md`.
- [431](https://github.com/psylsph/home-energy-manager/issues/431), pausing an AIO behind a Gateway during a smart charge (v1.8.0). John's 9 November test was the car's own unplanned plug-in charge, so the hold rightly stayed out. Waiting on his log from an overnight slot; he can also try the #434 top-up.
- [446](https://github.com/psylsph/home-energy-manager/issues/446) (v1.8.4), waiting on Brian to try the next Saving Session and confirm the export stops where it says and Minimum SOC comes back afterwards. The floor that reaches the 23:30 cheap start is #405.
- [452](https://github.com/psylsph/home-energy-manager/issues/452), the "Open Dongle Page" button (v1.8.7, see `docs/issue-452-dongle-page-plan.md`). Jet-bundle has confirmed the page loads from a normal home network, after a few retries; open until they have tried the button and told us the login and dongle generation.
- [406](https://github.com/psylsph/home-energy-manager/issues/406), dispatch slots as known load (v1.6.0 and v1.7.0). The last comment there still says #434 isn't built, so Brian could be told it is.
- [433](https://github.com/psylsph/home-energy-manager/issues/433), the battery pause on the Gen1, Gen2, Gen3 and AC models (v1.10.0, v1.10.1). Open until a Gen1 owner has tried it; its one code leftover is in Tier 1.

Items that have shipped are left off the tables below; Stuart closes the issues once the reporter confirms.

## Tier 1: bugs and user-facing gaps

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [458](https://github.com/psylsph/home-energy-manager/issues/458) | EV charging label shows Charging at 0 W | S | **Fixed in v1.14.1:** the EV label now says Charging only when the flow line is drawn (power above the noise threshold), whatever the charger's state register says, so Brian's bridge reporting Charging at 0 W with no cable reads Connected. Open until Brian confirms. His bridge still reports state, cable and power from different sources, worth making consistent on his side. Kept trade-off: a real charger whose car has paused mid-session also reads Connected / Cable In. |
| 2 | [433](https://github.com/psylsph/home-energy-manager/issues/433) | Battery pause: the unread-pause flag on the HR 300-359 models | S | v1.10.0 and v1.10.1 gave the Gen1, Gen2, Gen3 and AC models the pause GivTCP gives them, and the Gen1 4xx says "HEM can't read the pause" instead of showing a stale state. Still not covered: models that read the pause through the HR 300-359 block (the AC-coupled and the All-in-Ones) never raise that flag when the block fails, so they can show the last state as fact. Small and ready. HEM's own pause on the AC three-phase stays, recorded as a deliberate difference. |
| 3 | [456](https://github.com/psylsph/home-energy-manager/issues/456) | Read house figures from the Gateway while controlling the All-in-One directly | L | The Gateway keeps its own copy of the schedule and battery registers (#428, closed), so HEM points at the AIO to control the battery, but the AIO can't see the house: its grid and load readings describe its link to the Gateway, so Home comes out 0 or negative-derived and the day totals are impossible (John's AIO claimed 16 kWh exported while discharging 1 kWh). The right figures are in the Gateway's input registers, which HEM already decodes when pointed at one, so the fix is a second read-only poll and one merged snapshot with each field from the device that measures it. It touches every page that shows Home, the history rows, the daily report and the planner's consumption profile, so it needs the simulator's Gateway + AIO pairing and John's raw snapshots to settle which registers mean what. Large, and waiting on those readings. |

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
| 1 | [453](https://github.com/psylsph/home-energy-manager/issues/453) | Planner: plan a charge in every cheap period, not just the cheapest | L | Split from #448. The Planner and its automatic refresh each pick one window, so a tariff like Cosy's three cheap periods gets one charge a day. Planning each period means telling the planner which window it is for, firing the refresh once per period, and rethinking how the three strategies and the recurring charge slot behave. The design pass is written up: a toggle that is off by default keeps existing installs unchanged, and checking more often would not help on its own because the limits are the once-a-day latch and the single-window pick. The open questions are settled (same-rate windows, skip a late window, toggle beside the strategy selector, slot 1 only), so it is ready to build. Cosy mode already charges in every window, so the cost of waiting is low: peterb99, who asked, has said they'll use Cosy mode for now (7 November), with the maximum charge at 90% to leave room for solar. The detailed plan is in the `docs` directory (`docs/issue-453-every-cheap-window-plan.md`). |
| 2 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one: it gives the EV charger a place in the house story, and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |
| 3 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | Moved to the bottom on purpose. Only one person has asked for it, and it changes how tariffs are stored and validated, which feeds History costs, the planner and the tariff automations, so a mistake could break existing installs. If it is picked up, keep it additive: existing saved tariffs must load and price exactly as before, new options such as a base rate with override windows or windows that cross midnight must be opt-in, and the old and new forms need tests that compare costs side by side. Revisit if more people ask. |

## Dependencies at a glance

```text
#428 Gateway keeps its own schedule copy (shipped) ─► #456 house figures from the Gateway while controlling the AIO
#446 held floor during a window (shipped in v1.8.4) ─► #405 calculated floor writes through it
#407 back-stop ─► #405 export surplus
#406 dispatch as known load (shipped) ─► #405 export surplus, which can't export while the car charges
```

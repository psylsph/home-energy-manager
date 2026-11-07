# Roadmap

Priority order for the open GitHub issues, as of 7 November 2026 (v1.8.6). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs three things: whether users are affected today, whether a later item depends on it, and how much it costs. Where it differs from the issue's own label, the reason is given.

## Tier 0: active now

Nothing is active. Issues 437 and 448 (Planner setup, from discussion #436) shipped in v1.8.5 and v1.8.6 and are closed; the replan idea from that thread is #453 (Tier 5). Items that have shipped are left off; some of those issues stay open until the reporter confirms the fix, and Stuart closes them.

## Tier 2: bugs and user-facing gaps

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [452](https://github.com/psylsph/home-energy-manager/issues/452) | Point the connection-problem message at the dongle's restart page | S | A user was told to power-cycle the isolator when restarting the WiFi dongle from its own web page would have done. HEM's reboot goes over the connection that is already down by the time the message appears, so a button for it can't work. The plan is an "Open Dongle Page" button on the `AwaitingConnection` message that opens `http://<inverter-ip>/home.html` (Device Management, then Restart), with the isolator advice kept as the fallback. Waiting on the reporter to say whether that page loads on a normal home network, what login it wants, and which dongle generation it is; if it only works on the inverter's own WiFi, the wording changes. The detailed plan is in the `docs` directory (`docs/issue-452-dongle-page-plan.md`). |
| 2 | [427](https://github.com/psylsph/home-energy-manager/issues/427) | Adding a second battery when master/slave are swapped: existing battery data | M | Real data stays attached to the wrong battery after a swap. Battery health history is keyed by stack position, not serial. The planned fix records the serial with each day and follows the battery when it moves. Until then, the reporter's manual SQL fix is in the issue and the FAQ. Per-battery history is only a few weeks old, so the cost of waiting is low, but it is a real defect in a feature that was just released. |
| 3 | [433](https://github.com/psylsph/home-energy-manager/issues/433) | Battery pause: offer it on the models GivTCP does | M, then L | GivTCP pauses the Gen1 (ARM 187+, mode only), the Gen2 and Gen3 (below ARM 312) and the AC Mk2 (ARM 200+), which HEM doesn't yet, so Timed Discharge and the Charge Cap are missing there. HEM also gives the AC three-phase a pause that GivTCP doesn't, and the Gen4, three-phase and EMS are on the simulator's pause list when GivTCP has none. Each gap is recorded in `tests/fixtures/givtcp-model-contract.json`, so the tests fail once one is fixed, as the reminder to drop it. The HV Gen3 and AIO Hybrid gained their pause with the single-phase schedule work (v1.8.2), so they are no longer part of this. The Gen1 4xx can't read HR 318 back, so the charge cap and the limiters must cope with no read-back there. |
| 4 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one: it gives the EV charger a place in the house story, and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |
| 5 | [426](https://github.com/psylsph/home-energy-manager/issues/426) | Charge cap: the reporter's case isn't covered | S, then unknown | The cap shipped in v1.7.1, but the reporter's reply on 6 November says it doesn't fit: their battery sits at 100% from late morning because a separate Solax inverter charges it from a 12 kW array, not from the grid, and they don't want to export it all or dump the battery. The cap only pauses HEM's side of the charge path, and HEM can't see or limit the Solax. Next step is a reply saying what the cap can and can't do for AC-coupled solar into an AIO, and asking whether the cap plus a lower export setting is enough. Don't build a new mechanism before that answer. |

## Tier 3: smarter planning around Intelligent Octopus Go

These come from #403 (now closed), where Brian and Stuart split the original discussion into separate tickets. They share one theme (the planner should know what the car and the Octopus dispatch list are doing), so they are best done in this order. #434 is the exception: it builds on the #431 hold work, which has shipped but is waiting for the reporter to confirm it on their AIO.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [404](https://github.com/psylsph/home-energy-manager/issues/404) | Re-plan and notify when Octopus moves a dispatch slot | M | Octopus keeps moving slots through the evening, so a plan sized against an old slot goes stale. It reuses the dispatch-as-load handling from #406 and the existing "what the automatic plan did" reporting. The ownership rules for the shared Eco Paused state must keep winning over a running smart charge. |
| 2 | [434](https://github.com/psylsph/home-energy-manager/issues/434) | Charge the battery during Intelligent Octopus Go smart charging | M | An opt-in on the Octopus page with a target SOC, reusing the Force Charge path and the hold's ownership rules. Open questions are in the issue: a supply cap (about 11.5 kW was seen with the car and one AIO charging), and which dispatches count. Start once the reporter has confirmed the #431 hold on their AIO, so the hold is proven first. |
| 3 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. The original post is still ambiguous; confirm it with the reporter before building. |
| 4 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The riskiest item here. The simulation it needs already exists as the Planner's export advice (`plan_export_window`), which projects hourly solar and consumption to the next cheap import window — but it sizes a duration, and a Saving Session's window is Octopus's, so this needs the inverse: the floor that leaves the house covered until 23:30. #446 (shipped in v1.8.4) built the thing it writes to (a held Minimum SOC during a window), #407 is the back-stop for a thin projection; it also builds on the dispatch-as-load handling from #406 and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |

## Tier 4: protecting users and widening reach

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [397](https://github.com/psylsph/home-energy-manager/issues/397) | Detect a dongle factory reset, add a security checklist | M | Labelled low priority, but with GivEnergy in administration no firmware fix is coming. A reset wipes the schedule silently, and the checklist half is cheap, passive documentation. It must not probe the dongle's other ports. The checklist can have its own Settings section. |
| 2 | [396](https://github.com/psylsph/home-energy-manager/issues/396) | MQTT output with Home Assistant auto-discovery | L | Probably the largest group of potential users, and it gives them one poller on the dongle instead of two. Publish only at first. Control over MQTT later, through the same safety rules as the authenticated control API. The main server's bind and CORS stay as they are. |

## Tier 5: later

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [398](https://github.com/psylsph/home-energy-manager/issues/398) | Tariff comparison: what would last year have cost? | L | A popular question. It no longer waits on #132: it prices against the public Octopus product rates, with the existing tariff editor supplying the "what I actually paid" baseline. Start by re-pricing actual import and export, then re-run the planner's simulation per tariff. |
| 2 | [392](https://github.com/psylsph/home-energy-manager/issues/392) | Import history from the GivEnergy Cloud | L | Needs a GivEnergy Premium subscription, and the 402 for a free key hasn't been confirmed. The cloud's future is uncertain, so it may become impossible, but it is only worth building after someone confirms it works with a real key. |
| 3 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | Moved to the bottom on purpose. Only one person has asked for it, and it changes how tariffs are stored and validated, which feeds History costs, the planner and the tariff automations, so a mistake could break existing installs. If it is picked up, keep it additive: existing saved tariffs must load and price exactly as before, new options such as a base rate with override windows or windows that cross midnight must be opt-in, and the old and new forms need tests that compare costs side by side. Revisit if more people ask. |
| 4 | [453](https://github.com/psylsph/home-energy-manager/issues/453) | Planner: plan a charge in every cheap period, not just the cheapest | L | Split from #448. The Planner and its automatic refresh each pick one window, so a tariff like Cosy's three cheap periods gets one charge a day. Planning each period means telling the planner which window it is for, firing the refresh once per period, and rethinking how the three strategies and the recurring charge slot behave. Needs a design pass first; a toggle that is off by default keeps existing installs unchanged. Cosy mode already charges in every window, so the cost of waiting is low. |

## Dependencies at a glance

```text
#431 pause via AIO pause-discharge (shipped in v1.8.0) ─► #434 charge during dispatch
#446 held floor during a window (shipped in v1.8.4) ─► #405 calculated floor writes through it
#407 back-stop ─► #405 export surplus
```

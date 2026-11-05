# Roadmap

Priority order for the open GitHub issues, as of 4 November 2026 (v1.7.2). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs three things: whether users are affected today, whether a later item depends on it, and how much it costs. Where it differs from the issue's own label, the reason is given.

## Tier 0: active now

Issue 448 comes first: it is a small documentation fix that sends users to the wrong place today. Work in progress in the tree (uncommitted, 35+ files) appears to cover #431 and #429, so those follow.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [448](https://github.com/psylsph/home-energy-manager/issues/448) | Planner setup: where tariff windows and charging mode go (from discussion #436) | S | Users are being pointed at the wrong settings. The help text is already fixed in the working tree, and the FAQ section on charging mode and tie-breaks is written, but both are uncommitted. Still to do: the Timed Charge hint on the Status page. The 22:00–00:00 pick is explained, not a bug: the Planner ranks same-rate windows later today first, then the longer one (see the FAQ and the correction on discussion #436). Also covers the replan-after-each-cheap-period idea from the thread. |
| 2 | [431](https://github.com/psylsph/home-energy-manager/issues/431) | Batteries not paused during Octopus smart charging slots (AIO behind a Gateway) | M | A confirmed bug affecting a real user today: the battery keeps covering the car during a smart charge. The reporter's logs show the reserve-100% write is refused by the AIO. The in-progress fix pauses through the AIO's own pause-discharge setting instead, and restores the previous values when the charge ends. It also stops retrying a failing write every poll, reports a failed pause on the Octopus page, and lets Force Charge run during a hold. Still needed: the test build the reporter asked for, a reply to the reporter's questions (the Gateway parallel-mode and IP questions), and a reminder that the reserve was left at 100% during testing. Keep the ownership rules intact. |
| 3 | [437](https://github.com/psylsph/home-energy-manager/issues/437) | Planner setup gaps: tariff help text says History-only, and automatic planning never says it needs Standard mode | S | A documentation bug that leads people away from working features. The help text change is already in the working tree. What remains is the planner message about Standard mode, with tests. Small enough to ship alongside #431. Its report came from discussion #436, which is now tracked by #448, so ship it together with #448. |
| 4 | [429](https://github.com/psylsph/home-energy-manager/issues/429) | Today tiles → In/Out balance bars | S | Mostly built (`EnergyBalanceBars`, `lib/energyBalance.ts`, the `SummaryTiles` removal, and their tests). Finish, check the e2e specs that find the old tile text, and close. |

## Tier 2: bugs and user-facing gaps

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [432](https://github.com/psylsph/home-energy-manager/issues/432) | AIO Hybrid and HV Gen3: use the single-phase schedule registers, as GivTCP does | L | A confirmed bug on the AIO Hybrid (`0x82xx`) and HV Gen3 (`0x81xx`): a saved charge slot writes HR 1113/1114 and HR 242, reads back fine, and never charges because `enable_charge` stays off. It touches every schedule control on both models, so it is test-first and needs the simulator changed to match. It is ahead of #433 because #433 depends on it for these two models. |
| 2 | [427](https://github.com/psylsph/home-energy-manager/issues/427) | Adding a second battery when master/slave are swapped: existing battery data | M | Real data stays attached to the wrong battery after a swap. Battery health history is keyed by stack position, not serial. The planned fix records the serial with each day and follows the battery when it moves. Until then, the reporter's manual SQL fix is in the issue and the FAQ. Per-battery history is only a few weeks old, so the cost of waiting is low, but it is a real defect in a feature that was just released. |
| 3 | [433](https://github.com/psylsph/home-energy-manager/issues/433) | Battery pause: offer it on the models GivTCP does | M, then L | GivTCP pauses the Gen1 (ARM 187+), Gen2, AC Mk2 (ARM 200+), HV Gen3 and AIO Hybrid, which HEM doesn't yet, so Timed Discharge and the Charge Cap are missing there. HEM also gives the AC three-phase a pause that GivTCP doesn't, and the Gen4, three-phase and EMS are on the simulator's pause list when GivTCP has none. Each gap is recorded in `tests/fixtures/givtcp-model-contract.json`, so the tests fail once one is fixed, as the reminder to drop it. Depends on #432 for the HV Gen3 and AIO Hybrid. The Gen1 4xx can't read HR 318 back, so the charge cap and the limiters must cope with no read-back there. |
| 4 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one: it gives the EV charger a place in the house story, and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |

## Tier 3: smarter planning around Intelligent Octopus Go

These come from #403 (now closed), where Brian and Stuart split the original discussion into separate tickets. They share one theme (the planner should know what the car and the Octopus dispatch list are doing), so they are best done in this order. #434 is the exception: it builds on the #431 hold work and should wait for it to ship.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [404](https://github.com/psylsph/home-energy-manager/issues/404) | Re-plan and notify when Octopus moves a dispatch slot | M | Octopus keeps moving slots through the evening, so a plan sized against an old slot goes stale. It reuses the dispatch-as-load handling from #406 and the existing "what the automatic plan did" reporting. The ownership rules for the shared Eco Paused state must keep winning over a running smart charge. |
| 2 | [434](https://github.com/psylsph/home-energy-manager/issues/434) | Charge the battery during Intelligent Octopus Go smart charging | M | An opt-in on the Octopus page with a target SOC, reusing the Force Charge path and the hold's ownership rules. Open questions are in the issue: a supply cap (about 11.5 kW was seen with the car and one AIO charging), and which dispatches count. Start only once #431 has shipped, so the hold is proven first. |
| 3 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. The original post is still ambiguous; confirm it with the reporter before building. |
| 4 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window (also covers [446](https://github.com/psylsph/home-energy-manager/issues/446): Saving Session exports draining the battery before the cheap start) | L | The riskiest item here. Saving Session exports currently run down to a fixed floor with no check against the cheap start, so #446's case is one of the things this must handle. It needs #407 first, builds on the dispatch-as-load handling from #406, and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |

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

## Dependencies at a glance

```text
#431 pause via AIO pause-discharge (in progress) ─► #434 charge during dispatch
#407 back-stop ─► #405 export surplus
#432 single-phase schedule on AIO Hybrid / HV Gen3 ─► #433 pause on the same models
```

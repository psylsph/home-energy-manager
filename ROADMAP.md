# Roadmap

Priority order for the open GitHub issues, as of 5 November 2026 (v1.8.1). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs three things: whether users are affected today, whether a later item depends on it, and how much it costs. Where it differs from the issue's own label, the reason is given.

## Tier 0: active now

Issue 450 comes first: it is a bug that tells users a join failed when it probably worked. #448 and #437 each have a small piece left. #431 and #429 shipped in v1.8.0 and stay here only until they can be closed.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [450](https://github.com/psylsph/home-energy-manager/issues/450) | Saving Sessions: an 'already signed up' refusal is reported as a failed join | S | A user with auto-join on gets a "could not join" alert for an event they are in, because HEM keeps only Octopus's message and drops its error code and reason (`KT-GB-5117`, "already signed up to this event"). The fix keeps the code and reason in the alert and the Octopus page, treats that refusal as probably joined, and re-reads the sessions on a later poll before calling it a failure, since the 6-second confirmation window can miss a slow Octopus. The joined list and the join call disagreeing on the reporter's account is worth a look but needn't block the alert fix. |
| 2 | [448](https://github.com/psylsph/home-energy-manager/issues/448) | Planner setup: where tariff windows and charging mode go (from discussion #436) | S | The Settings help text and the FAQ section on charging mode and tie-breaks shipped in v1.8.0. Still to do: the Timed Charge hint on the Status page. The 22:00–00:00 pick is explained, not a bug: the Planner ranks same-rate windows later today first, then the longer one (see the FAQ and the correction on discussion #436). Also covers the replan-after-each-cheap-period idea from the thread. |
| 3 | [437](https://github.com/psylsph/home-energy-manager/issues/437) | Planner setup gaps: tariff help text says History-only, and automatic planning never says it needs Standard mode | S | The help text is fixed and shipped in v1.8.0. What remains is the Planner message about Standard mode, with tests. Its report came from discussion #436, which #448 tracks, so ship it together with #448. |
| 4 | [431](https://github.com/psylsph/home-energy-manager/issues/431) | Batteries not paused during Octopus smart charging slots (AIO behind a Gateway) | S | Shipped in v1.8.0: the hold pauses through the AIO's own pause-discharge setting and puts the previous values back. It stays open until the reporter has tried it on his AIO and Stuart closes it. Pause failing outside a smart charge was ruled out as the hold's fault on the issue. |
| 5 | [429](https://github.com/psylsph/home-energy-manager/issues/429) | Today tiles → In/Out balance bars | S | Shipped in v1.8.0. Check that the e2e specs no longer look for the old tile text, then close. |

## Tier 2: bugs and user-facing gaps

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [446](https://github.com/psylsph/home-energy-manager/issues/446) | Saving Session export empties the battery before the cheap start | S, then M | Investigated 2026-11-05. The reporter's AC 3.0 (`0x3001`, one discharge slot) has no per-slot floor register — those start at Gen3's HR 272+ — and the panel only offers a floor above two slots, so his export ran to whatever Minimum SOC said: 4%. Nothing told him that, and the booking wrote no floor of its own. Two stages: show where the export will actually stop, in the panel and in the session messages (S); then let the booking hold a floor in HR 110 for its window and put it back (M), which is the developer-mode Discharge Floor Guard promoted to a real owner and also covers two-slot hybrids. The guard must stop restoring the reserve over another owner's safety pause before it ships. The calculated floor that leaves enough for the house until 23:30 stays in #405. |
| 2 | [452](https://github.com/psylsph/home-energy-manager/issues/452) | Offer an inverter reboot from the connection-problem message | S | A user was told to power-cycle the isolator when a software reboot from the inverter's own WiFi page would have done. HEM already has a reboot control (developer-mode Control page) and the connection-problem screen (`AwaitingConnection`) already walks through the power-cycle, so this is a pointer to the gentler option, or a button where the write path can reach the inverter. Cheap, but a reboot needs a connection that isn't dropping, so settle that before adding a button. |
| 3 | [427](https://github.com/psylsph/home-energy-manager/issues/427) | Adding a second battery when master/slave are swapped: existing battery data | M | Real data stays attached to the wrong battery after a swap. Battery health history is keyed by stack position, not serial. The planned fix records the serial with each day and follows the battery when it moves. Until then, the reporter's manual SQL fix is in the issue and the FAQ. Per-battery history is only a few weeks old, so the cost of waiting is low, but it is a real defect in a feature that was just released. |
| 4 | [433](https://github.com/psylsph/home-energy-manager/issues/433) | Battery pause: offer it on the models GivTCP does | M, then L | GivTCP pauses the Gen1 (ARM 187+, mode only), the Gen2 and Gen3 (below ARM 312) and the AC Mk2 (ARM 200+), which HEM doesn't yet, so Timed Discharge and the Charge Cap are missing there. HEM also gives the AC three-phase a pause that GivTCP doesn't, and the Gen4, three-phase and EMS are on the simulator's pause list when GivTCP has none. Each gap is recorded in `tests/fixtures/givtcp-model-contract.json`, so the tests fail once one is fixed, as the reminder to drop it. The HV Gen3 and AIO Hybrid gained their pause with the single-phase schedule work (v1.8.2), so they are no longer part of this. The Gen1 4xx can't read HR 318 back, so the charge cap and the limiters must cope with no read-back there. |
| 5 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one: it gives the EV charger a place in the house story, and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |

## Tier 3: smarter planning around Intelligent Octopus Go

These come from #403 (now closed), where Brian and Stuart split the original discussion into separate tickets. They share one theme (the planner should know what the car and the Octopus dispatch list are doing), so they are best done in this order. #434 is the exception: it builds on the #431 hold work and should wait for it to ship.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [404](https://github.com/psylsph/home-energy-manager/issues/404) | Re-plan and notify when Octopus moves a dispatch slot | M | Octopus keeps moving slots through the evening, so a plan sized against an old slot goes stale. It reuses the dispatch-as-load handling from #406 and the existing "what the automatic plan did" reporting. The ownership rules for the shared Eco Paused state must keep winning over a running smart charge. |
| 2 | [434](https://github.com/psylsph/home-energy-manager/issues/434) | Charge the battery during Intelligent Octopus Go smart charging | M | An opt-in on the Octopus page with a target SOC, reusing the Force Charge path and the hold's ownership rules. Open questions are in the issue: a supply cap (about 11.5 kW was seen with the car and one AIO charging), and which dispatches count. Start only once #431 has shipped, so the hold is proven first. |
| 3 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. The original post is still ambiguous; confirm it with the reporter before building. |
| 4 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The riskiest item here. The simulation it needs already exists as the Planner's export advice (`plan_export_window`), which projects hourly solar and consumption to the next cheap import window — but it sizes a duration, and a Saving Session's window is Octopus's, so this needs the inverse: the floor that leaves the house covered until 23:30. #446 builds the thing it writes to (a held reserve during a window), #407 the back-stop for a thin projection; it also builds on the dispatch-as-load handling from #406 and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |

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
#431 pause via AIO pause-discharge (shipped in v1.8.0) ─► #434 charge during dispatch
#446 held floor during a window ─► #405 calculated floor writes through it
#407 back-stop ─► #405 export surplus
```

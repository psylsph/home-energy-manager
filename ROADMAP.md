# Roadmap

Priority order for the open GitHub issues, as of 1 November 2026 (v1.4.0). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs three things: whether users are affected today, whether a later item depends on it, and how much it costs. Where it differs from the issue's own label, the reason is given.

## Tier 1: finish what has already shipped

The code is done for all of these. They stay open only until a release or a reply, so closing them off keeps the list honest.

| # | Issue | Effort | What is left |
|---|---|---|---|
| [378](https://github.com/psylsph/home-energy-manager/issues/378) | Long term backup of History.db | S | Scheduled backups shipped (#379). Restore steps were posted on the issue (30 Oct) and are now in `INSTALL.md`, linked from the issue (31 Oct). The reporter has thanked us and is reworking their own backup script around it (31 Oct), so nothing is left on HEM's side. Ready to close. |
| [419](https://github.com/psylsph/home-energy-manager/issues/419) | Solar Graph puzzle | S | Fixed in v0.97.0. The overnight climb was the inverter's output counter standing in for solar while the panel counters read zero, not a clock problem; solar now comes only from the panel counters, so the drop at sunrise and the inflated Generated total are gone. Days already recorded keep their old values. Explained on the issue (31 Oct); waiting for the reporter to confirm on v0.97.0, then close. |

## Tier 2: user-facing gaps and polish

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [426](https://github.com/psylsph/home-energy-manager/issues/426) | Optional charge cap: stop the battery charging above a set SOC | M | A user with AIOs behind a Gateway only fails over to island mode when the battery isn't full (their installer traced it to an AC-coupled Solax), and with GivEnergy gone no firmware fix is coming. The open question is how to stop charging through a Gateway, which has no pause registers; a zero charge rate needs confirming on real hardware first. It also needs a periodic full charge so the cells still balance (#418's calibration cycle could serve). |
| 2 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one — it gives the EV charger a place in the house story (the post is currently bare), and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |

## Tier 3: smarter planning around Intelligent Octopus Go

These come from #403 (now closed), where Brian and Stuart split the original discussion into separate tickets. They share one theme (the planner should know what the car and the Octopus dispatch list are doing), so they are best done in this order.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [406](https://github.com/psylsph/home-energy-manager/issues/406) | Use Octopus dispatch slots as known load | M | The dispatch list is already polled every 3 minutes for the battery hold, so the data exists. Its profile half builds on #400, now done. Decide whether a smart charge outside the cheap window counts as peak-rate load. The reporter also suggests using an EV slot's cheap rate to charge the house battery. That is worth its own ticket once this lands. |
| 2 | [404](https://github.com/psylsph/home-energy-manager/issues/404) | Re-plan and notify when Octopus moves a dispatch slot | M | Octopus keeps moving slots through the evening, so a plan sized against an old slot goes stale. It reuses #406's dispatch-as-load handling and the existing "what the automatic plan did" reporting. The ownership rules for the shared Eco Paused state must keep winning over a running smart charge. |
| 3 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has now said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. |
| 4 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The riskiest item here. It needs #406 and #407 first (#400 is done), and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |

## Tier 4: protecting users and widening reach

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [397](https://github.com/psylsph/home-energy-manager/issues/397) | Detect a dongle factory reset, add a security checklist | M | Labelled low priority, but with GivEnergy in administration no firmware fix is coming. A reset wipes the schedule silently, and the checklist half is cheap, passive documentation. It must not probe the dongle's other ports. The Settings split (#402) is done, so the checklist can have its own Settings section. |
| 2 | [396](https://github.com/psylsph/home-energy-manager/issues/396) | MQTT output with Home Assistant auto-discovery | L | Probably the largest group of potential users, and it gives them one poller on the dongle instead of two. Publish only at first. Control over MQTT later, through the same safety rules as the authenticated control API. The main server's bind and CORS stay as they are. |

## Tier 5: later

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [398](https://github.com/psylsph/home-energy-manager/issues/398) | Tariff comparison: what would last year have cost? | L | A popular question. It no longer waits on #132: it prices against the public Octopus product rates, with the existing tariff editor supplying the "what I actually paid" baseline. Start by re-pricing actual import and export, then re-run the planner's simulation per tariff. |
| 2 | [392](https://github.com/psylsph/home-energy-manager/issues/392) | Import history from the GivEnergy Cloud | L | Needs a GivEnergy Premium subscription, and the 402 for a free key hasn't been confirmed. The cloud's future is uncertain, so it may become impossible, but it is only worth building after someone confirms it works with a real key. |
| 3 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | Moved to the bottom on purpose. Only one person has asked for it, and it changes how tariffs are stored and validated, which feeds History costs, the planner and the tariff automations, so a mistake could break existing installs. If it is picked up, keep it additive: existing saved tariffs must load and price exactly as before, new options such as a base rate with override windows or windows that cross midnight must be opt-in, and the old and new forms need tests that compare costs side by side. Revisit if more people ask. |

## Dependencies at a glance

```text
#400 EV out of profile (done) ─► #406 dispatch as load ─► #404 re-plan on moved slot
                                       └─────────► #405 export surplus ◄── #407 back-stop
#402 Settings split (done) ─► #397 reset detection + checklist, #396 MQTT
```

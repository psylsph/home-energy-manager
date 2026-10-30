# Roadmap

Priority order for the open GitHub issues, as of 30 October 2026 (v0.95.0). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs three things: whether users are affected today, whether a later item depends on it, and how much it costs. Where it differs from the issue's own label, the reason is given.

## Tier 1: finish what has already shipped

These need little or no new code. Closing them off keeps the list honest.

| # | Issue | Effort | What is left |
|---|---|---|---|
| [378](https://github.com/psylsph/home-energy-manager/issues/378) | Long term backup of History.db | S | Scheduled backups shipped (#379). The reporter's last question is how to restore one, and the docs don't say. Add restore steps to `INSTALL.md` (stop HEM, replace `history.db`, remove stale `-wal`/`-shm`), then close. |
| [359](https://github.com/psylsph/home-energy-manager/issues/359) | Automatic planning options | S | All three strategies shipped (Keep a minimum, Hold through the cheap window, Cheapest overall). Two things remain: the reporter's question about the planner's solar chart showing kWh rather than kW, and their feedback on Cheapest overall. Answer the first, then close once the second arrives. |

## Tier 2: user-facing gaps and groundwork

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | The oldest open issue (June) and it affects every user with a real supplier tariff. The editor locks the first start and last end and forces filler windows, which makes History costs easy to get wrong. A base rate plus override windows, and windows that cross midnight, fix it. Presets (Flux, Cosy, Go, Economy 7) are a natural follow-on. |
| 2 | [393](https://github.com/psylsph/home-energy-manager/issues/393) | Battery health tracking | M | The daily summary and the Battery page chart shipped in v0.95.0. Still to do: alerts when cell spread keeps widening or SOC jumps or sticks (the BMS 3007–3022 "SOC bug"), and the guided calibration cycle. Warranties are no longer honoured, so early warning matters. Close the issue once the alerts land and split calibration into its own ticket if it slips. |
| 3 | [400](https://github.com/psylsph/home-energy-manager/issues/400) | Leave EV charging out of the consumption profile | S–M | Marked low priority, but I'd move it up. It makes the planner over-charge today on days the car wasn't plugged in, and #406 builds directly on it. Use the GivEnergy EV charger's power where HEM has it, with a "load above X kW" threshold otherwise. |
| 4 | [402](https://github.com/psylsph/home-energy-manager/issues/402) | Break up the Settings page | L | A pure refactor, but `SettingsPage.tsx` is about 2,900 lines and the save-without-blur bugs (#391) came from copied save logic. Do it before #397 and #396 each add another Settings section. Work in the order the issue suggests (Panel Controls and Notifications first), with the test suite green after each step. Do the section components and the shared save hook first, and the navigation rail after. |

## Tier 3: smarter planning around Intelligent Octopus Go

These come from #403, where Brian and Stuart split the original discussion into separate tickets. They share one theme (the planner should know what the car and the Octopus dispatch list are doing), so they are best done in this order.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [406](https://github.com/psylsph/home-energy-manager/issues/406) | Use Octopus dispatch slots as known load | M | The dispatch list is already polled every 3 minutes for the battery hold, so the data exists. It needs #400 first for the profile half. Decide whether a smart charge outside the cheap window counts as peak-rate load. The reporter also suggests using an EV slot's cheap rate to charge the house battery. That is worth its own ticket once this lands. |
| 2 | [404](https://github.com/psylsph/home-energy-manager/issues/404) | Re-plan and notify when Octopus moves a dispatch slot | M | Octopus keeps moving slots through the evening, so a plan sized against an old slot goes stale. It reuses #406's dispatch-as-load handling and the existing "what the automatic plan did" reporting. The ownership rules for the shared Eco Paused state must keep winning over a running smart charge. |
| 3 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has now said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. |
| 4 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The riskiest item here. It needs #400, #406 and #407 first, and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |
| — | [403](https://github.com/psylsph/home-energy-manager/issues/403) | Features, Docs, Road map | — | Discussion issue. Its asks are now #404–#407, so it can be closed once Stuart is happy that nothing is lost. |

## Tier 4: protecting users and widening reach

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [397](https://github.com/psylsph/home-energy-manager/issues/397) | Detect a dongle factory reset, add a security checklist | M | Labelled low priority, but with GivEnergy in administration no firmware fix is coming. A reset wipes the schedule silently, and the checklist half is cheap, passive documentation. It must not probe the dongle's other ports. Best after #402 so the checklist gets a proper Settings section. |
| 2 | [396](https://github.com/psylsph/home-energy-manager/issues/396) | MQTT output with Home Assistant auto-discovery | L | Probably the largest group of potential users, and it gives them one poller on the dongle instead of two. Publish only at first. Control over MQTT later, through the same safety rules as the authenticated control API. The main server's bind and CORS stay as they are. |

## Tier 5: later

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [398](https://github.com/psylsph/home-energy-manager/issues/398) | Tariff comparison: what would last year have cost? | L | A popular question, and it depends on #132 for faithful tariff definitions. Start by re-pricing actual import and export, then re-run the planner's simulation per tariff. |
| 2 | [392](https://github.com/psylsph/home-energy-manager/issues/392) | Import history from the GivEnergy Cloud | L | Needs a GivEnergy Premium subscription, and the 402 for a free key hasn't been confirmed. The cloud's future is uncertain, so it may become impossible, but it is only worth building after someone confirms it works with a real key. |
| 3 | [401](https://github.com/psylsph/home-energy-manager/issues/401) | Show grid carbon intensity | S–M | Nice to have, and nobody has asked for it. A display-only version (current and forecast intensity) is cheap. Letting the planner prefer greener hours should wait until the planner changes above settle. |

## Dependencies at a glance

```text
#132 tariff editor ──────────────► #398 tariff comparison
#400 EV out of profile ─► #406 dispatch as load ─► #404 re-plan on moved slot
                                       └─────────► #405 export surplus ◄── #407 back-stop
#402 Settings split ─────► #397 reset detection + checklist, #396 MQTT
```

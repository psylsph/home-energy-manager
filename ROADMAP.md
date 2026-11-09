# Roadmap

Priority order for the open GitHub issues, as of 9 November 2026 (v1.11.0). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

The order weighs four things: whether users are affected today, whether a later item depends on it, how much it costs, and whether it is waiting on someone else's answer. Cheap items that unblock a user come first, and large or risky items come after the ones that are ready to build. Where it differs from the issue's own label, the reason is given.

## Tier 0: active now

Nothing is being built right now. [455](https://github.com/psylsph/home-energy-manager/issues/455), Energy Tariffs from Octopus and the rate recorded on every history row, shipped in v1.11.0. [454](https://github.com/psylsph/home-energy-manager/issues/454), recording car charging separately, shipped in v1.9.0 and is closed. v1.9.1 (a dark-mode colour fix for the Today bars), v1.10.0 (the battery pause on more models, #433) and v1.10.1 (the "can't read the pause" notes) are released and their builds passed. Left over from #455, none of it blocking: an automatic fill of missing History rates after the first Octopus rates sync (today it is a button), the Telegram daily report still prices at the current tariff, smart-charge periods outside Intelligent Octopus Go's 23:30 to 05:30 still show at the day rate, and nothing yet reads the new gas rates. Shipped but still open, waiting on the reporter to confirm:

- [427](https://github.com/psylsph/home-energy-manager/issues/427), battery health history following a battery that moves slot (v1.8.8). Brian has the SQL to move his pre-update days across; no reply since.
- [452](https://github.com/psylsph/home-energy-manager/issues/452), the "Open Dongle Page" button (v1.8.7, see `docs/issue-452-dongle-page-plan.md`). Jet-bundle has confirmed the page loads at the inverter's address from a normal home network, though sometimes only after a few "server not found" retries, so the button should work for them. It stays open until they have tried it and told us what login it wants and which dongle generation it is.
- [433](https://github.com/psylsph/home-energy-manager/issues/433), the battery pause on the Gen1, Gen2, Gen3 and AC models (v1.10.0). Open until a Gen1 owner has tried it.
- [431](https://github.com/psylsph/home-energy-manager/issues/431), pausing an AIO behind a Gateway during a smart charge (v1.8.0). John replied on 8 November that he's been travelling and will test a smart charge at home on 9 November; #434 waits on that.
- [429](https://github.com/psylsph/home-energy-manager/issues/429), the Today balance bars. Brian's feedback (square ends, "Today's Energy In / Out" titles) went out in v1.8.4, and v1.9.1 fixes the dark-mode text colour.
- [446](https://github.com/psylsph/home-energy-manager/issues/446) (v1.8.4), [432](https://github.com/psylsph/home-energy-manager/issues/432) (v1.8.2) and [406](https://github.com/psylsph/home-energy-manager/issues/406), dispatch slots as known load (shipped; #404 and #405 build on it).

The replan idea from the Planner setup thread (discussion #436) is #453 (Tier 5). Items that have shipped are left off the tables below; Stuart closes the issues once the reporter confirms.

## Tier 1: bugs and user-facing gaps

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [433](https://github.com/psylsph/home-energy-manager/issues/433) | Battery pause: what's left after v1.10.1 | S | v1.10.0 gave the Gen1 (ARM 187+, mode only), the Gen2, the Gen3 on any firmware and both AC models (ARM 200+) the pause GivTCP gives them, so Timed Discharge and the Charge Cap now appear there. The two leftovers shipped in v1.10.1: the Gen1 4xx can't read HR 318 back, so the Charge Cap and Timed Discharge now say "HEM can't read the pause" instead of showing the last state as fact, and the simulator serves the pause only where GivTCP does (the Gen1 from ARM 187 on the mode alone, none on the Gen4, three-phase and EMS), with those `sim_deviations` gone from the contract. The simulator side (register store, the Gen1 engine fix, a cross-crate pause test, the contract copy) is released as simulator v0.18.7. Still not covered: models that read the pause through the HR 300-359 block (the AC-coupled and the All-in-Ones) never raise the flag when that block fails. HEM's own pause on the AC three-phase stays, recorded as a deliberate difference. Leave the issue open until the Gen1 owner reports. |

## Tier 2: smarter planning around Intelligent Octopus Go

These come from the #403 discussion, where Brian and Stuart split the original thread into separate tickets. They share one theme (the planner should know what the car and the Octopus dispatch list are doing). #404 comes first because it is the smaller, ready piece. #434 is the exception: it builds on the #431 hold work, which has shipped but is waiting for the reporter to confirm it on their AIO, so it moves up as soon as they do. The export-advice pair from the same thread (#407, #405) is larger and riskier, so it sits in Tier 4, after the work that protects and widens the user base.

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [404](https://github.com/psylsph/home-energy-manager/issues/404) | Re-plan and notify when Octopus moves a dispatch slot | M | Octopus keeps moving slots through the evening, so a plan sized against an old slot goes stale. It reuses the dispatch-as-load handling from #406 and the existing "what the automatic plan did" reporting. The ownership rules for the shared Eco Paused state must keep winning over a running smart charge. |
| 2 | [434](https://github.com/psylsph/home-energy-manager/issues/434) | Charge the battery during Intelligent Octopus Go smart charging | M | An opt-in on the Octopus page with a target SOC, reusing the Force Charge path and the hold's ownership rules. Open questions are in the issue: a supply cap (about 11.5 kW was seen with the car and one AIO charging), and which dispatches count. Start once the reporter has confirmed the #431 hold on their AIO, so the hold is proven first. |

## Tier 3: protecting users and widening reach

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [397](https://github.com/psylsph/home-energy-manager/issues/397) | Detect a dongle factory reset, add a security checklist | M | Labelled low priority, but with GivEnergy in administration no firmware fix is coming. A reset wipes the schedule silently, and the checklist half is cheap, passive documentation. It must not probe the dongle's other ports. The checklist can have its own Settings section. |
| 2 | [396](https://github.com/psylsph/home-energy-manager/issues/396) | MQTT output with Home Assistant auto-discovery | L | Probably the largest group of potential users, and it gives them one poller on the dongle instead of two. Publish only at first. Control over MQTT later, through the same safety rules as the authenticated control API. The main server's bind and CORS stay as they are. |

## Tier 4: Planner export advice

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | Planner minimum battery level as an overridable floor | M | The reporter has said what they meant: a back-stop that stops optimised exporting from emptying the battery before the cheap window, tightened as HEM learns the household's evening discharge rate. That is a guard for #405 rather than a change to the existing floor, so design it with #405 and don't ship it alone. The original post is still ambiguous; confirm it with the reporter before building. |
| 2 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The riskiest item here. The simulation it needs already exists as the Planner's export advice (`plan_export_window`), which projects hourly solar and consumption to the next cheap import window — but it sizes a duration, and a Saving Session's window is Octopus's, so this needs the inverse: the floor that leaves the house covered until 23:30. #446 (shipped in v1.8.4) built the thing it writes to (a held Minimum SOC during a window), #407 is the back-stop for a thin projection; it also builds on the dispatch-as-load handling from #406 and cannot export while the car is charging. Start as Planner advice (as the other export advice is) and consider acting on its own only after real-world use. |

## Tier 5: later

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [398](https://github.com/psylsph/home-energy-manager/issues/398) | Tariff comparison: what would last year have cost? | L | A popular question. It no longer waits on #132: it prices against the public Octopus product rates, with the existing tariff editor supplying the "what I actually paid" baseline. Start by re-pricing actual import and export, then re-run the planner's simulation per tariff. |
| 2 | [453](https://github.com/psylsph/home-energy-manager/issues/453) | Planner: plan a charge in every cheap period, not just the cheapest | L | Split from #448. The Planner and its automatic refresh each pick one window, so a tariff like Cosy's three cheap periods gets one charge a day. Planning each period means telling the planner which window it is for, firing the refresh once per period, and rethinking how the three strategies and the recurring charge slot behave. The design pass is written up: a toggle that is off by default keeps existing installs unchanged, and checking more often would not help on its own because the limits are the once-a-day latch and the single-window pick. The open questions are settled (same-rate windows, skip a late window, toggle beside the strategy selector, slot 1 only), so it is ready to build. Cosy mode already charges in every window, so the cost of waiting is low: peterb99, who asked, has said they'll use Cosy mode for now (7 November), with the maximum charge at 90% to leave room for solar. The detailed plan is in the `docs` directory (`docs/issue-453-every-cheap-window-plan.md`). |
| 3 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | Picked up from gagenp01's ideas on the closed #422. The garage half is the useful one: it gives the EV charger a place in the house story, and the charger's offline states decide what gets drawn. A re-skin of the same anchors, so the flow model and tests don't move. Styles are breadth afterward. Supplier logos stay out (third-party branding). |
| 4 | [392](https://github.com/psylsph/home-energy-manager/issues/392) | Import history from the GivEnergy Cloud | L | Needs a GivEnergy Premium subscription, and the 402 for a free key hasn't been confirmed. The cloud's future is uncertain, so it may become impossible, but it is only worth building after someone confirms it works with a real key. |
| 5 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | Moved to the bottom on purpose. Only one person has asked for it, and it changes how tariffs are stored and validated, which feeds History costs, the planner and the tariff automations, so a mistake could break existing installs. If it is picked up, keep it additive: existing saved tariffs must load and price exactly as before, new options such as a base rate with override windows or windows that cross midnight must be opt-in, and the old and new forms need tests that compare costs side by side. Revisit if more people ask. |

## Dependencies at a glance

```text
#431 pause via AIO pause-discharge (shipped in v1.8.0) ─► #434 charge during dispatch
#446 held floor during a window (shipped in v1.8.4) ─► #405 calculated floor writes through it
#407 back-stop ─► #405 export surplus
#454 car charging (shipped in v1.9.0) ─► #455 stamped rates and Energy Tariffs from Octopus (shipped in v1.11.0)
```

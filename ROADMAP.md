# Roadmap

Priority order for the open GitHub issues, as of 17 November 2026 (v1.18.1). Effort is a rough guide: **S** is a day or less, **M** a few days, **L** a week or more.

Every open issue is either waiting on someone outside (listed in Tier 0) or has a plan, linked in its row. The plans are local working notes in `docs/` (gitignored). The issues raised in the last two days came first (Tier 1, now all built), then the older planned work. Within a tier, cheap items that are ready to build come before ones that still need a decision from Stuart.

## Tier 0: shipped or waiting on someone else

Closed since the last pass: #474 (the car's planned charges in the Next Charge card, v1.17.0) and #475 (sunlight as a word, v1.17.0), both Stuart's own; #469 (Octopus's API host move, v1.16.1), #404 (the replan, v1.14.0), #429 (the Today balance bars), #432, #455 (Energy Tariffs from Octopus), #457 (Home without the car), #431 (John confirmed on 12 November that the overnight smart charge held the AIO behind his Gateway; his logs are on the issue), #434 (the smart-charge top-up, v1.13.0; it has not yet seen a real daytime dispatch on Stuart's 0x3001, which should record its ARM firmware; plan in `docs/issue-434-dispatch-charge-plan.md`; not built: Octopus's monthly car-allowance limit), #433 (the battery pause on the models GivTCP gives it, finished in v1.16.0; the AC three-phase keeps its Timed Discharge as a recorded difference), #452 (the Open Dongle Page button: Jet-bundle reports no drop-outs since, so silence counts as success), and #392 and #398, which came off the list. #460 (the What's New link to the changelog, v1.15.0) is closed, and so is #396 (the MQTT output with Home Assistant discovery), which Stuart closed on 12 November as low value; it comes off the Tier 2 table. Left over from #455, none of it blocking: an automatic fill of missing History rates after the first Octopus rates sync (today it is a button), the Telegram daily report still prices at the current tariff, and nothing yet reads the new gas rates. #404 is closed but hasn't yet seen a real night of moved slots; if one goes wrong, reopen it (plan in `docs/issue-404-replan-plan.md`).

Shipped, waiting on the reporter (each was told on 14 November, when v1.17.0 went out):

- [473](https://github.com/psylsph/home-energy-manager/issues/473), names for the CT meters (BrianUK6, v1.17.0) and the follow-up for his missing third meter (v1.17.4). Settings → Meters, after Solar Arrays; the Meters page and both meter pickers use the meter's own name, then its Solar Arrays name, then the address, which stays visible under a name. His meter 0x03 vanished after the upgrade because HEM checked each meter address once per connection; it now keeps looking, one missing address at a time every 2 minutes for any meter that has a name or a solar array, and for the rest every 2 minutes for 30 minutes and then every 15, and a named or solar-array meter that isn't answering shows a "Not responding" card. Kept trade-off: a system with no meters pays one 3 s probe now and then in a poll cycle for as long as HEM runs. Brian still needs a reply: does a restart bring 0x03 back on 1.17.3, is it a solar array, and how often does his own reader poll the dongle. Plan in `docs/issue-473-meter-names-plan.md` (follow-up section).
- [472](https://github.com/psylsph/home-energy-manager/issues/472), "Empty at 05:30" in the Battery Level card (BrianUK6, v1.17.0). Panel Controls → Show Battery Runtime, house view, off by default. It runs the Planner's own forecast from the live SOC for a median, a busy and a quiet day, rounded to the half hour, with one to three dots and an ⓘ for how close they agree. It says "Lasts until the 23:30 charge" when a slot, a #434 top-up or the sun refills it first, "Full at 04:10" while charging, and nothing while discharge is paused. Kept trade-offs: with weather off, solar counts as nothing and confidence is capped at Medium; a schedule window starting inside the current hour is judged over the whole hour. Brian's 16 November report ("Lasts until solar at 10:30" at 10:47 while discharging 667 W) is fixed in v1.17.6: the live load now counts for the rest of the hour as the live sun does, a discharging battery keeps draining at its rate, a time is never rounded back into the past (and a cached one that has passed is rebuilt), and the battery's direction is in the cache key. Kept trade-offs: a brief load spike counts for the rest of the hour until the next rebuild, and a refill already under way reads as the next half hour. He still needs a reply (what it was; watch a few sunny mornings with a heavy load on). Plan in `docs/issue-472-battery-runtime-plan.md`.
- [471](https://github.com/psylsph/home-energy-manager/issues/471), read-only links (BrianUK6, v1.17.0). `?RO` hides Control, Settings and Octopus from the nav and sends their typed addresses back to Status, from one list; the Planner keeps its forecast without Apply or Plan Settings, and links to the hidden pages become plain text. Plan in `docs/issue-471-read-only-plan.md`.
- [470](https://github.com/psylsph/home-energy-manager/issues/470), saying when a charge-slot hold ends (git-boballen, v1.17.0).
- [468](https://github.com/psylsph/home-energy-manager/issues/468), the "Overnight charge" wording on Cosy's daytime windows (v1.16.1). peterb99 was told it shipped on 14 November. Kept trade-off: the day word looks only at the window's start, so a morning window that's already running can still read "Tonight".
- [453](https://github.com/psylsph/home-energy-manager/issues/453), a charge before every cheap period (v1.16.0). Waiting on peterb99 to try Plan Every Cheap Window on Cosy. Kept trade-off: a period HEM wakes into more than 10 minutes late isn't planned. Plan in `docs/issue-453-every-cheap-window-plan.md`.
- [446](https://github.com/psylsph/home-energy-manager/issues/446) (v1.8.4), waiting on Brian's next Saving Session to confirm that the export stops where it says and that Minimum SOC comes back. The floor that reaches the 23:30 cheap start is #405.

Reporter looks satisfied, so these are ready to close with Stuart's say-so (both are BrianUK6's):

- [458](https://github.com/psylsph/home-energy-manager/issues/458), the EV label saying Charging at 0 W (v1.14.1). On 13–14 November Brian said the states now read correctly, and that the remaining timing came from his Zappi bridge, which he has since fixed.
- [406](https://github.com/psylsph/home-energy-manager/issues/406), dispatch slots as known load (v1.6.0, v1.7.0). On 13 November Brian said the #434 top-up "works a marvellous treat" and he has removed his own workaround.

Waiting on readings from the reporter:

- [456](https://github.com/psylsph/home-energy-manager/issues/456), reading house figures from the Gateway while controlling the All-in-One directly (**L**). The AIO can't see the house, so Home comes out 0 and the day totals are impossible. The fix is a second read-only poll of the Gateway, merged into one snapshot. On 12 November John was asked whether he wants this and for AIO snapshots (idle, car charging, battery charging), one Gateway snapshot, the number of AIOs, and whether anything else polls the Gateway. Not started until he replies. Approach in the issue and its comments.

Items that have shipped are left off the tables below; Stuart closes the issues once the reporter confirms.

## Tier 1: raised in the last two days

All six from the 14 November batch shipped in v1.17.0 (#470 to #475; see Tier 0), #473's follow-up in v1.17.4 and #472's follow-up in v1.17.6. The one issue raised since, #479 (Stuart's own, 16 November), shipped in v1.18.1: every forecast floors the battery one point above its reserve, where a cold battery really stops, so the Forecast page's projections, the Planner and the battery runtime card agree with the inverter, and a battery at reserve + 1 reads "At Reserve". "Below Reserve" and the flash stay on the real reserve, a reserve of 100 is untouched, and nothing written to the inverter changes. Kept trade-offs: the margin is a fixed point for every battery, warm or cold; a reserve of 99 reads as a battery that can't discharge. Plan in `docs/issue-479-reserve-margin-plan.md`. Nothing is waiting here; #479 is ready for Stuart to close.

To check the car line by hand, `npm run mock:octopus -- --with-hem --slot 23:30-05:30@12 --running 40` serves made-up charges to a throwaway HEM at <http://127.0.0.1:7340> (see CONTRIBUTING.md).

## Tier 2: protecting users

Issue #397 (dongle factory-reset detection and the security checklist) shipped in v1.18.0. Nothing else is waiting here.

## Tier 3: Planner export advice

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [407](https://github.com/psylsph/home-energy-manager/issues/407) | A back-stop floor for exports before the cheap window | M | Brian settled the intent on 26 October: the floor always wins, and the forecast never overrides it. The floor is what the house needs until the cheap window (p75 consumption, EV hours excluded, less p25 solar) plus a margin, never below the Planner minimum or the reserve. Build it first because #405 depends on it. Open decisions (Stuart deferred them): the margin, and whether it also guards Saving Session exports (#446). Plan in `docs/issue-405-407-export-floor-plan.md`. |
| 2 | [405](https://github.com/psylsph/home-energy-manager/issues/405) | Export surplus charge before the cheap window | L | The inverse of the existing export advice: work back from 23:30 to find when a full-rate export must start to land on #407's floor, with no export while a smart dispatch runs. Advice on the Planner first. Acting on it (through #446's held-floor path) comes only after real use. The riskiest item here. Same plan as #407. |

## Tier 4: later

| Order | # | Issue | Effort | Why here |
|---|---|---|---|---|
| 1 | [425](https://github.com/psylsph/home-energy-manager/issues/425) | House view: a garage for the EV charge point and a choice of house styles | M, then L | The garage first: the EV cable ends on its wall instead of the post, so the flow model and its tests don't move. The charger's offline states decide whether it's lit, dark or absent. Garage styles next, then house styles on the same anchors. No supplier logos. Open decisions (Stuart deferred them): whether to ship the garage alone, and where the style choice goes. Plan in `docs/issue-425-garage-plan.md`. |
| 2 | [132](https://github.com/psylsph/home-energy-manager/issues/132) | Energy Tariffs: don't hard-code 00:00/23:59 | M | Last on purpose. Only one person has asked, #455 now reads rates from Octopus, and tariffs feed History costs, the planner and the tariff automations. If it's built, it's additive: a standard rate with special windows (crossing midnight allowed) resolved by one function into today's tiling, so every saved tariff prices exactly as before. Decision: whether it's worth doing yet (recommended: revisit if more people ask). Plan in `docs/issue-132-tariff-windows-plan.md`. |

## Dependencies at a glance

```text
#428 Gateway keeps its own schedule copy (shipped) ─► #456 house figures from the Gateway while controlling the AIO
#446 held floor during a window (shipped in v1.8.4) ─► #405 calculated floor writes through it
#407 back-stop floor ─► #405 export surplus (planned together)
#406 dispatch as known load (shipped) ─► #405 export surplus, which can't export while the car charges
```

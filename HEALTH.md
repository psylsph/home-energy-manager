# HEM Health API

`GET /api/health` tells a monitoring tool or a status display whether Home Energy Manager (HEM) is working. This page is for anyone writing a client for it: a dashboard tile, an Uptime Kuma check, a script on another Pi, or an LLM asked to build one. The first half explains it in plain terms. The [reference](#reference) after that gives every field, value and rule, plus a [JSON Schema](#json-schema).

For setting up a monitor without writing code, see [INSTALL.md → Checking HEM is healthy from another machine](./INSTALL.md#checking-hem-is-healthy-from-another-machine).

## In short

```bash
curl -s http://solarpi:7337/api/health
```

- **Port and host:** the same as the web UI, normally port `7337`.
- **No key, no login, read-only.** The answer contains only statuses, times and short fixed codes: no serial numbers, settings, account details or error text.
- **Cheap.** It never talks to the inverter, the internet or the database. It just reads what HEM already knows, so asking every minute is fine.
- **One overall answer** in `status`: `healthy`, `degraded` or `unhealthy`.
- **HTTP status:** `200` for `healthy` and `degraded`, `503` for `unhealthy`. A tool that only checks the status code still gets the important part right.
- **One entry per part of HEM** under `checks`, each with its own `status` and, where it helps, a `detail` code and when that part last worked.

## What the three answers mean

| `status` | HTTP | Meaning | What a client should do |
|---|---|---|---|
| `healthy` | 200 | Everything that's set up is working. | Show green. |
| `degraded` | 200 | HEM is doing its main job, but something needs a look: a backup failed, a notification didn't get through, disk space is low, the weather is out of date. Also shown for a few minutes after HEM starts. | Show amber, and list the checks that aren't `ok`. Alert only if it lasts (half an hour is a sensible default). |
| `unhealthy` | 503 | A core part has stopped: HEM has no fresh data from the inverter, its polling has stopped, or history isn't being saved. | Show red and alert. |

Only three checks are **core** (`"core": true`), and only they can make HEM `unhealthy`:

- **`inverter`**: HEM has a reading from the inverter that's less than three polls old (at least 3 minutes). A dongle that drops and reconnects quickly doesn't fail it.
- **`poll_loop`**: HEM's polling is still going round, even while the dongle isn't answering. This tells a stuck HEM apart from a dongle that is just offline.
- **`history`**: readings are still being saved. This catches a full disk or a worn-out SD card.

The other six (`backup`, `disk`, `notifications`, `weather`, `solar_forecast`, `octopus`) can only make it `degraded`. A feature that isn't set up says `disabled` and never counts against the answer.

## An example

A HEM with the dongle offline for twenty minutes. Polling is fine and history has nothing waiting, so only the inverter check fails:

```json
{
  "status": "unhealthy",
  "version": "1.22.1",
  "uptime_s": 86400,
  "now": 1763800000,
  "checks": {
    "inverter":       { "status": "fail", "core": true,  "detail": "reconnecting", "last_ok": 1763798800, "age_s": 1200 },
    "poll_loop":      { "status": "ok",   "core": true,  "last_ok": 1763799999, "age_s": 1 },
    "history":        { "status": "ok",   "core": true,  "last_ok": 1763798820, "age_s": 1180 },
    "backup":         { "status": "ok",   "core": false, "last_ok": 1763760000, "age_s": 40000 },
    "disk":           { "status": "ok",   "core": false, "free_mb": 12000 },
    "notifications":  { "status": "disabled", "core": false },
    "weather":        { "status": "ok",   "core": false, "last_ok": 1763799100, "age_s": 900 },
    "solar_forecast": { "status": "ok",   "core": false, "last_ok": 1763790000, "age_s": 10000 },
    "octopus":        { "status": "disabled", "core": false }
  }
}
```

A display might show: **HEM: Unhealthy. Inverter: no data for 20 minutes (reconnecting).**

## Writing a client

1. **Fetch** `GET /api/health` every 60 seconds with a timeout of about 10 seconds. Any status code is a real answer: read the body on a `503` too.
2. **Treat no answer as its own state.** A timeout, a refused connection or a body that isn't JSON means HEM itself (or the machine, or the network) is down. That's worse than `unhealthy`, and the endpoint can't report it, so the client has to.
3. **Show `status`** as the headline, then the checks that aren't `ok` or `disabled`, using the [check names](#checks) and [detail codes](#detail-codes) below as labels.
4. **Show times as ages:** "2 minutes ago" from `age_s` reads better than a timestamp, and doesn't depend on the client's clock agreeing with HEM's.
5. **Be tolerant.** Ignore checks, fields and detail codes you don't recognise, and treat an unknown check `status` like `warn`. New checks may be added in later versions; the existing names and meanings won't change.

A minimal JavaScript client:

```js
async function hemHealth(base = "http://solarpi:7337") {
  try {
    const res = await fetch(`${base}/api/health`, { signal: AbortSignal.timeout(10_000), cache: "no-store" });
    const body = await res.json();
    const problems = Object.entries(body.checks)
      .filter(([, c]) => c.status !== "ok" && c.status !== "disabled")
      .map(([name, c]) => `${name}: ${c.status}${c.detail ? ` (${c.detail})` : ""}`);
    return { state: body.status, problems, version: body.version };
  } catch (e) {
    return { state: "unreachable", problems: [String(e)] };
  }
}
```

And in Python:

```python
import requests

def hem_health(base="http://solarpi:7337"):
    try:
        body = requests.get(f"{base}/api/health", timeout=10).json()
    except (requests.RequestException, ValueError) as e:
        return {"state": "unreachable", "problems": [str(e)]}
    problems = [
        f"{name}: {c['status']}" + (f" ({c['detail']})" if "detail" in c else "")
        for name, c in body["checks"].items()
        if c["status"] not in ("ok", "disabled")
    ]
    return {"state": body["status"], "problems": problems, "version": body["version"]}
```

---

## Reference

### Request

| | |
|---|---|
| Method | `GET` (`HEAD` also answers, with the same status code and no body) |
| Path | `/api/health` |
| Port | HEM's web port, `7337` by default (`--port` in headless mode) |
| Authentication | None |
| Query parameters | None |
| Cross-origin | Allowed from any origin, so a browser page served elsewhere can call it |
| Caching | The response carries `Cache-Control: no-store` |
| Cost | No inverter reads, network calls or database queries |

### Response

`Content-Type: application/json`. HTTP `200` when `status` is `healthy` or `degraded`, `503` when it is `unhealthy`.

| Field | Type | Meaning |
|---|---|---|
| `status` | string | `healthy`, `degraded` or `unhealthy` ([rules](#how-the-overall-status-is-worked-out)) |
| `version` | string | HEM's version, e.g. `"1.22.1"` |
| `uptime_s` | integer ≥ 0 | Seconds since HEM started |
| `now` | integer | HEM's clock when it answered, Unix seconds (UTC) |
| `checks` | object | One [check object](#check-object) per check, keyed by [check name](#checks). All nine are always present. |

### Check object

| Field | Type | Present | Meaning |
|---|---|---|---|
| `status` | string | always | `ok`, `warn`, `fail`, `starting` or `disabled` |
| `core` | boolean | always | `true` for `inverter`, `poll_loop` and `history`, which are the only checks that can make the overall status `unhealthy` |
| `detail` | string | sometimes | A fixed snake_case code saying what the check found ([codes](#detail-codes)). Never free text. |
| `last_ok` | integer | sometimes | When this part last worked, Unix seconds (UTC). Left out when it never has. |
| `age_s` | integer ≥ 0 | with `last_ok` | Seconds since `last_ok`, worked out by HEM. Prefer it over `now - last_ok`. |
| `free_mb` | integer ≥ 0 | `disk` only | Free space on the drive holding HEM's history, in MiB. Left out when it couldn't be read. |

Check statuses:

| `status` | Meaning |
|---|---|
| `ok` | Working |
| `warn` | Needs a look; makes the overall status `degraded` |
| `fail` | Not working. On a core check this makes the overall status `unhealthy`; on any other check, `degraded` |
| `starting` | HEM started less than five minutes ago and this part hasn't reported yet; makes the overall status `degraded` |
| `disabled` | Not set up; never affects the overall status |

### How the overall status is worked out

1. `unhealthy` if any check with `"core": true` is `fail`.
2. Otherwise `degraded` if any check is `warn`, `fail` or `starting`.
3. Otherwise `healthy`.

### Checks

`N` below is HEM's poll interval in seconds (60 by default, set in Settings).

| Check | Core | What it watches | `last_ok` is |
|---|---|---|---|
| `inverter` | yes | How old HEM's latest inverter reading is, whatever the connection state. Fails once it's older than `max(3 × N, 180)` seconds. While HEM is busy writing settings to the inverter, the reading's age stops growing for up to 15 minutes, because HEM reads only after the writes finish. Past that, polling counts as stuck and the age is the real one again. | The latest reading's time. Kept through an outage, so `age_s` says how long HEM has been without data. |
| `poll_loop` | yes | Whether HEM's polling is still running. Each time round, HEM says how long until it next checks in (up to 15 minutes in a poll cycle or a connect, 2 minutes while waiting) and fails if it misses that. | The latest check-in |
| `history` | yes | Whether readings reach the history database. Fails once a reading has waited more than `max(2 × N + 120, 300)` seconds to be written. A single failed write only warns, since the next minute's write usually works. Writes that keep failing while readings keep coming fail once that wait runs out; time with nothing to write (the inverter unreachable) doesn't count. | The latest successful write |
| `backup` | no | Scheduled history backups (Settings → History Backup). With an interval of `H` hours: warns if the latest attempt failed, if the last success is more than `2 × H + 1` hours ago, or if there has never been one once backups have been on for `H + 1` hours. | The latest successful backup |
| `disk` | no | Free space on the drive holding the history database. Warns under 500 MiB, fails under 100 MiB. | Not given |
| `notifications` | no | Telegram, ntfy and Pushover, each one you've set up. Warns while any channel's latest message failed; saving new details for a channel clears it. Alerts and test messages count; replies to Telegram bot commands and report documents don't. Disabled when alerts are off or no channel is set up. | The latest delivered message on any channel |
| `weather` | no | Live weather (Settings → Weather), fetched every 15 minutes. Warns when the last fetch is over an hour old. Disabled without weather turned on and a location. | The latest fetch |
| `solar_forecast` | no | The solar forecast, fetched every 3 hours with the weather. Warns when the last one stored is over 7 hours old. Disabled like `weather`. | The latest forecast stored |
| `octopus` | no | Octopus account sync, every 6 hours. Warns when the latest sync failed (the account couldn't be read, or no meter's data could be fetched) or the last good one is over 13 hours old. One meter or the tariff failing on its own doesn't count, since the rest still synced. Disabled without an Octopus API key and account number. | The latest good sync |

A check that has never succeeded isn't counted against HEM straight away. `backup`, `weather`, `solar_forecast` and `octopus` say `not_yet` (status `ok`) until the feature has been on for as long as it would take to count as overdue or stale. That's timed from when HEM started, or from when you switched the feature on. A feature switched back on gets the same time even if it has an old success from before it was off.

### Detail codes

Every value `detail` can take, by check. A check with no code in a given state leaves `detail` out.

| Check | `status` | `detail` | Meaning |
|---|---|---|---|
| `inverter` | `ok` | `connected` | Fresh reading, connected now |
| `inverter` | `ok` | `reconnecting`, `disconnected` | Fresh reading, but the connection just dropped (a quick reconnect doesn't fail) |
| `inverter` | `starting` | `connected`, `reconnecting`, `disconnected` | No reading yet in the first five minutes; the code is the connection state |
| `inverter` | `fail` | `stale_reading` | Connected, but no new reading for too long |
| `inverter` | `fail` | `reconnecting`, `disconnected` | Not connected, and no reading for too long |
| `inverter` | `fail` | `no_reading` | No reading since HEM started, after the first five minutes |
| `inverter` | `fail` | `not_configured` | No inverter address entered (no start-up grace) |
| `poll_loop` | `ok`, `starting` | none | Running, or not started yet in the first five minutes |
| `poll_loop` | `fail` | `stuck` | Missed its check-in |
| `poll_loop` | `fail` | `not_running` | Never started, after the first five minutes |
| `history` | `ok` | none, or `idle` | Writing; `idle` when nothing has been written yet (no readings so far) |
| `history` | `warn` | `write_failed` | The latest write failed |
| `history` | `fail` | `not_written` | Readings have waited too long to be written |
| `history` | `fail` | `unavailable` | The history database isn't open (no start-up grace) |
| `backup` | `ok` | none, or `not_yet` | Up to date, or no backup yet but not overdue |
| `backup` | `warn` | `last_failed` | The latest backup attempt failed |
| `backup` | `warn` | `overdue` | The last success is too long ago |
| `backup` | `warn` | `never` | No successful backup since they were turned on |
| `disk` | `ok` | none, or `unavailable` | Enough space, or free space couldn't be read |
| `disk` | `warn` | `low_space` | Under 500 MiB free |
| `disk` | `fail` | `very_low_space` | Under 100 MiB free |
| `notifications` | `ok` | none, or `none_sent` | Delivering, or nothing sent yet |
| `notifications` | `warn` | `last_failed` | A channel's latest message didn't get through |
| `weather`, `solar_forecast`, `octopus` | `ok` | none, or `not_yet` | Up to date, or no update yet but not overdue |
| `weather`, `solar_forecast`, `octopus` | `warn` | `stale` | The last update is too old |
| `weather`, `solar_forecast`, `octopus` | `warn` | `never` | No update since the feature was turned on |
| `octopus` | `warn` | `last_failed` | The latest sync failed (account unreadable, or no meter's data fetched); stays while the next sync runs, and clears when you save new Octopus details |
| any | `disabled` | none | Not set up |

### Times and clocks

- `now` and every `last_ok` are Unix seconds from HEM's clock.
- HEM measures the ages it judges by on a clock that NTP corrections and other jumps can't move. So a status stays right even when HEM's wall clock is stepped (a Raspberry Pi booting with an old time, for example), and `age_s` can differ from `now - last_ok` while that settles. Use `age_s` for display.
- The exception is `backup`: its last success is kept across restarts, so it's aged on the wall clock, and a large clock step can make it read `overdue` (or hide that) until the next backup.
- `age_s` is never negative.

### Stability

- The nine check names, the fields above and the meaning of every listed value are stable.
- Later versions may add checks, fields or detail codes. Clients should ignore anything they don't recognise, and treat an unknown check `status` like `warn`.
- `version` is HEM's own version, not an API version.

### JSON Schema

A JSON Schema (draft 2020-12) for the response body, for validating answers or generating client types:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "HEM health report",
  "type": "object",
  "required": ["status", "version", "uptime_s", "now", "checks"],
  "properties": {
    "status": { "enum": ["healthy", "degraded", "unhealthy"] },
    "version": { "type": "string" },
    "uptime_s": { "type": "integer", "minimum": 0 },
    "now": { "type": "integer", "description": "Unix seconds (UTC), HEM's clock" },
    "checks": {
      "type": "object",
      "required": ["inverter", "poll_loop", "history", "backup", "disk", "notifications", "weather", "solar_forecast", "octopus"],
      "properties": {
        "inverter": { "$ref": "#/$defs/check" },
        "poll_loop": { "$ref": "#/$defs/check" },
        "history": { "$ref": "#/$defs/check" },
        "backup": { "$ref": "#/$defs/check" },
        "disk": { "$ref": "#/$defs/check" },
        "notifications": { "$ref": "#/$defs/check" },
        "weather": { "$ref": "#/$defs/check" },
        "solar_forecast": { "$ref": "#/$defs/check" },
        "octopus": { "$ref": "#/$defs/check" }
      },
      "additionalProperties": { "$ref": "#/$defs/check" }
    }
  },
  "$defs": {
    "check": {
      "type": "object",
      "required": ["status", "core"],
      "properties": {
        "status": { "enum": ["ok", "warn", "fail", "starting", "disabled"] },
        "core": { "type": "boolean" },
        "detail": { "type": "string", "pattern": "^[a-z_]+$" },
        "last_ok": { "type": "integer", "description": "Unix seconds (UTC)" },
        "age_s": { "type": "integer", "minimum": 0 },
        "free_mb": { "type": "integer", "minimum": 0 }
      }
    }
  }
}
```

### Summary for LLMs and code generators

```text
ENDPOINT   GET http://<hem-host>:7337/api/health   (no auth, no params, CORS open, Cache-Control: no-store)
HTTP       200 = healthy|degraded, 503 = unhealthy; body is JSON in both cases; no answer = HEM unreachable
BODY       { status, version, uptime_s, now, checks: { <name>: { status, core, detail?, last_ok?, age_s?, free_mb? } } }
OVERALL    unhealthy if any core check is "fail"; else degraded if any check is warn|fail|starting; else healthy
CHECKS     core: inverter, poll_loop, history | non-core: backup, disk, notifications, weather, solar_forecast, octopus
STATUSES   ok | warn | fail | starting (first 5 min after start) | disabled (not set up; never counts)
TIMES      last_ok = Unix seconds UTC when that part last worked; age_s = seconds since (display this, not now - last_ok)
DETAILS    inverter: connected reconnecting disconnected stale_reading no_reading not_configured
           poll_loop: stuck not_running | history: idle write_failed not_written unavailable
           backup: not_yet last_failed overdue never | disk: unavailable low_space very_low_space (free_mb given)
           notifications: none_sent last_failed | weather/solar_forecast/octopus: not_yet stale never (+ octopus last_failed)
CLIENT     poll every 60 s, 10 s timeout; read the body on 503; ignore unknown checks/fields/details; unknown status = warn
DISPLAY    healthy=green, degraded=amber (alert only if it lasts ~30 min), unhealthy=red (alert), unreachable=red/grey
```

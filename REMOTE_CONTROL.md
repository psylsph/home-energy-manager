# Remote Battery Control API

Use Home Energy Manager's authenticated API to read battery status and trigger the same Quick Actions available in the app. Requests go to HEM, which communicates with the inverter; your script does not connect directly to Modbus.

**Start requests change real battery operation immediately.** Run the examples individually, not as one script. This API is not a future scheduler, and a successful request is not confirmation that the inverter has finished applying it.

## 1. Enable the API

1. Open **Settings → Remote / Mobile Network Access → Authenticated API**.
2. Click **Generate API key**. HEM creates a long random key and shows it **once** — copy it immediately. HEM stores only a verifier, so the secret cannot be read back from settings; generating a new key replaces the old one immediately.
3. Choose a port and listen address. The default port is **7338** and a fresh install listens on `127.0.0.1` (this machine only). Installs that were configured before these settings existed keep their all-interfaces behaviour until you set a listen address; HEM warns on that screen when it does.
4. **Apply network settings** rebinding the running listener immediately — no restart needed for port, listen address or browser-origin changes. Only *starting* the API for the first time (key + port saved on an old install) needs an app restart.
5. For charging/discharging commands, enable **Allow battery control through the authenticated API**. This toggle saves immediately; no restart is needed.

Reading status requires a valid key but **does not require battery-control permission**. A working status request does not prove that write permission is enabled. Starting an action requires it.

Disabling write permission prevents new external starts. An action you already started through this API can still be stopped remotely — revoking permission must never strand the inverter in a forced mode — but a credential without the permission gains nothing else. You can always use the app's Quick Actions buttons.

The API key, port, listen address, browser origins and the control toggle can only be changed **from the machine running HEM**. A visitor to the dashboard from another device cannot grant themselves a key or switch battery control on.

### Address and security

- On the machine running HEM: `http://localhost:7338`.
- From another device: use the HEM machine's LAN or VPN address, for example `http://192.168.1.100:7338` — this needs an all-interfaces (or LAN) listen address and still sends the bearer key over plain HTTP.
- `localhost` always means the machine executing the request, not necessarily the machine running HEM. Containers also have their own network context.
- Use your configured port if it differs from 7338. The main dashboard/API normally uses **7337**; these examples target the separate authenticated server.
- Every request needs `Authorization: Bearer <your-key>`. Do not put the key in a URL or query string.
- The recommended topology is **client → HTTPS via a trusted reverse proxy or VPN → HEM listening on `127.0.0.1`**. Plain HTTP does not encrypt the key, so direct LAN exposure is an explicit compatibility choice, not a secure channel. Do not expose this HTTP port to the public internet.
- Your generated key is long and random by construction. Keep it out of source control, screenshots, public webpages and shared logs.
- If a browser page must call the API directly, add its exact origin (e.g. `https://dashboard.example.com`) under **Allowed browser origins**. With no origins configured the API sends no CORS headers, which is what machine-to-machine integrations want.

### Known weaknesses and limits

Treat this API as a small, single-owner integration surface, not as a general identity or security system:

- There is one shared bearer key. There are no users, per-client permissions, key expiry or IP allow-lists. Anyone who obtains the key has the same read access, and—when the global control toggle is enabled—the same ability to start all Quick Actions. Failed sign-ins from one address are rate limited, and control starts are budgeted per key, but these are abuse brakes, not identity.
- The secret is shown once at generation and stored by HEM only as a verifier, but filesystem access to the HEM machine remains full control of the integration: protect the config directory and any backups you keep of it.
- `GET /api/snapshot` returns a deliberately limited operating view (power flows, state of charge, temperatures, grid readings, today's energy counters). It still reveals household energy behaviour, so grant read access only to systems that need it.
- A successful start response means HEM **accepted and queued** the command — not that the inverter has applied it. Every mutation returns a `command_id`; poll `GET /api/commands/{id}` for `readback_confirmed`, `failed`, `expired` or `unknown` before drawing conclusions.
- There is no emergency-stop guarantee. Remote stops are ordinary queued writes: if HEM or the inverter link is down, use the inverter's physical controls per the manufacturer's guidance.

The separate API does not expose settings or WebSocket endpoints. Enabling it does not change access to the main HEM server.

## 2. Endpoint reference

All paths below are relative to your authenticated server address.

| Method | Path | Request body | Battery-control permission |
|---|---|---|---|
| GET | `/api/control/status` | None | Not required |
| GET | `/api/snapshot` | None | Not required |
| GET | `/api/commands/{command_id}` | None | Not required |
| POST | `/api/control/force-charge` | `{"minutes":60}` | Required for new starts |
| POST | `/api/control/force-charge/stop` | None | See recovery stops below |
| POST | `/api/control/force-discharge` | `{"minutes":30}` | Required for new starts |
| POST | `/api/control/force-discharge/stop` | None | See recovery stops below |

Every POST needs two headers:

- `Authorization: Bearer <your-key>` — as always.
- `Idempotency-Key: <16-128 characters, no whitespace>` — a UUID is ideal. **Retries must reuse the same key**: a repeated request with the same key and payload returns the original response instead of queuing anything; the same key with a different payload is rejected with `409`. A missing or malformed key is rejected with `400`.

For starts, send `Content-Type: application/json` and **only** an integer `minutes` field, from **1 to 1439 inclusive**. Missing, duplicate or unknown fields, fractional numbers, strings, zero and out-of-range durations are rejected. There are no future start times, absolute end times, power or target-SOC overrides in this request format. If a start for the same action is already running, the response is `409` with the existing `command_id` rather than a competing command.

Stops use POST with no body. Opening a Stop URL in the browser address bar sends GET, not POST, and will not stop an action. A stop for an action you started through this API works even when the battery-control permission has since been switched off (recovery); with the permission off and no such action running, the stop is refused like any other control action.

Successful starts and stops include a `command_id` in the response. `GET /api/commands/{command_id}` returns its lifecycle state: `accepted` → `queued` → `dispatched` → `readback_confirmed`, or `failed`, `expired` or `unknown`. Confirm only on `readback_confirmed` — and treat `unknown` (for example after a HEM restart) as "go look", not "all good".

## 3. Curl examples

These examples use Bash or a compatible shell. Set these variables once in your terminal:

```bash
HEM_API='http://localhost:7338'
HEM_KEY='replace-with-your-key'
IDEM_KEY="$(uuidgen 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())')"
```

Use the LAN/VPN address instead of localhost when running curl on another machine. In Windows PowerShell, invoke `curl.exe` and substitute your URL and key directly, or use PowerShell's own variable syntax.

### Read battery status

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -H "Authorization: Bearer $HEM_KEY" \
  "$HEM_API/api/control/status"
```

For readable formatting, append `| jq`. To print only the summary:

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -H "Authorization: Bearer $HEM_KEY" \
  "$HEM_API/api/control/status" | jq -r '.summary'
```

`jq` is optional and must be installed separately. `--fail-with-body` needs curl 7.76.0 or newer; it preserves the error response while making HTTP errors return a nonzero exit code.

### Start Force Charge for 60 minutes

Generate a fresh `IDEM_KEY` for each *new* command; reuse it only to retry the same command:

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  -H "Idempotency-Key: $IDEM_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"minutes":60}' \
  "$HEM_API/api/control/force-charge"
```

An example acknowledgement is:

```json
{"ok":true,"message":"Force charge enabled","command_id":"1b2f3c4d5e6f"}
```

### Poll a command until the inverter confirms it

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -H "Authorization: Bearer $HEM_KEY" \
  "$HEM_API/api/commands/$COMMAND_ID"
```

Poll every 5–10 seconds with a bounded timeout until `data.state` is `readback_confirmed` (or handle `failed`/`expired`/`unknown`). HTTP acceptance only means queued.

### Stop Force Charge

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  -H "Idempotency-Key: $IDEM_KEY" \
  "$HEM_API/api/control/force-charge/stop"
```

An example acknowledgement is:

```json
{"ok":true,"message":"Force charge stopped","command_id":"9a8b7c6d5e4f"}
```

This acknowledges the handler's work; queued inverter writes and the next status reading can follow later. Poll the command id — `readback_confirmed` means the inverter has actually left the forced mode.

### Start Force Discharge for 30 minutes

Force Discharge is a forced-export Quick Action, not merely permission for the battery to supply the house. Stop an existing Force Charge before starting Force Discharge, and vice versa.

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  -H "Idempotency-Key: $IDEM_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"minutes":30}' \
  "$HEM_API/api/control/force-discharge"
```

### Stop Force Discharge

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  -H "Idempotency-Key: $IDEM_KEY" \
  "$HEM_API/api/control/force-discharge/stop"
```

### Read the inverter snapshot

Use this when you need detailed measurements beyond the battery operating summary. The response is a deliberately limited projection of live operating data — power flows, state of charge, temperatures, grid readings and today's energy counters — with `Cache-Control: no-store`. Internal identifiers, firmware details and per-module battery telemetry are never exposed; the response carries `observed_at` and `age_seconds` so you can reject stale data:

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -H "Authorization: Bearer $HEM_KEY" \
  "$HEM_API/api/snapshot"
```

## 4. Understand status and verify an action

`GET /api/control/status` reads HEM's cached state. It does not trigger a Modbus read or write. Responses include `Cache-Control: no-store`, but freshness is still limited by inverter polling.

A typical response while Force Charge is active contains these fields (other fields omitted here):

```json
{
  "ok": true,
  "summary": "Force Charge — charging; 56 minutes remaining",
  "mode": "eco",
  "activity": "charging",
  "control_source": "force_charge",
  "control_phase": "active",
  "remaining_minutes": 56,
  "quick_action": {
    "action": "force_charge",
    "phase": "active",
    "requested_at": "2026-09-08T17:11:41.296+00:00",
    "window_ends_at": "2026-09-08T18:11:41.296+00:00"
  },
  "schedules": {"charge": "active", "demand_discharge": "off", "export": "off"},
  "conditions": [],
  "connection": "connected",
  "stale": false,
  "observed_at": "2026-09-08T17:16:00+00:00",
  "age_seconds": 4,
  "stale_after_seconds": 60
}
```

### Status fields

| Field | Meaning |
|---|---|
| `ok` | A connected, sufficiently fresh inverter snapshot is available. Not a guarantee that the battery supports every action or is free of faults. |
| `summary` | Human-readable description. Display it, but do not parse its wording for automation. |
| `mode` | `eco`, `eco_paused`, `timed_demand`, `timed_export`, `export_paused`, or `unknown`. |
| `activity` | Observed `charging`, `discharging`, `idle`, or `unavailable`; not simply the requested action. |
| `control_source` | Best-known controller: `force_charge`, `force_discharge`, `timed_export`, `winter`, `cosy`, `agile`, `adaptive`, `timed_charge`, `inverter`, `safety`, or `unknown`. |
| `control_phase` | Controller-specific state, such as `pending`, `active`, `expired`, `waiting`, `observed` or `restricted`. Handle unrecognised values gracefully. |
| `quick_action` | Known HEM-owned force action, phase, request time and window end; otherwise `null`. Dates can be `null` if unavailable. |
| `remaining_minutes` | Known Quick Action window time remaining, rounded up; `0` after expiry, or `null` when unknown/not applicable. Not time until the battery is full or empty. |
| `schedules` | Separate `charge`, `export` and `demand_discharge` states: `off`, `armed`, `active` or `unknown`. `armed` can mean outside the window or not currently performing the action. |
| `automation` | Configuration and phases for Cosy, Agile, Adaptive Charge, winter, forecast and HEM-managed Timed Export; also `charging_mode`. |
| `conditions` | Simultaneous restrictions/warnings as `{code, label}` objects, including grid loss, trips, battery warnings, safety limits, pauses and unknown states. |
| `calibration` | Support flag, numeric stage and phase (for example `discharging`, `charging`, `balancing`, `finished`). |
| `maintenance` | Numeric mode and phase: `off`, `discharging`, `charging`, `standby` or `unknown`. |
| `limits` | Reserve/target SOC and raw charge/discharge rates; model-specific battery power cutoff where applicable. Raw rates are not watts or universally scaled percentages. |
| `connection` | `connected`, `disconnected` or `reconnecting`. |
| `stale` | Whether an existing reading is too old or implausibly future-dated. Not a connection flag. |
| `observed_at` | Snapshot timestamp in RFC 3339 format, or `null` before a reading exists. |
| `age_seconds` | Snapshot age, clamped to zero for future timestamps; `null` with no snapshot. |
| `stale_after_seconds` | Three polling intervals, with a minimum of 60 seconds. Readings over five seconds into the future are also stale. |

Mode, activity, schedules and controller are independent. Force Charge can operate in Eco mode. An active window may have idle activity because of SOC, limits or conditions. Discharging does not necessarily mean exporting to the grid. Schedule windows use the **inverter's clock**.

Safety restrictions can make `control_source` read `safety` while `quick_action` still describes an owned force action. Read both, together with `conditions`.

If `ok` is false, do not treat the result as current battery operation. Mode/activity are reported as unavailable/unknown and detail fields may be `null`. A disconnected inverter can have `stale:false` if its cached reading is recent. Before any reading exists, `observed_at:null`, `stale:false` and `ok:false` are expected. An empty `conditions` list in an unavailable response is not a clean bill of health.

### Verification sequence

1. Read status. Check `ok`, `connection`, `observed_at` and `conditions`.
2. Submit the intended action **once** with an `Idempotency-Key` and inspect its HTTP response. Keep the key — it is your retry ticket.
3. Poll the returned `command_id` (`GET /api/commands/{id}`) every 5–10 seconds with a bounded overall timeout. Faster HTTP polling does not speed up Modbus.
4. Treat `readback_confirmed` as applied; `queued`/`dispatched` as still in flight; `failed` as not done; `expired` as elapsed with confirming readback; `unknown` as **go look** — a HEM restart or lost link leaves honest uncertainty rather than a false success.
5. If readback does not match expectations, inspect HEM's UI/logs rather than repeatedly sending the command. A retry is only safe with the **same** key and payload — that replays the original response instead of queuing anything.

For example, stopping Force Charge may first acknowledge `Force charge stopped`, then show `Eco — discharging; timed charge armed` after readback. The armed schedule does not by itself mean Stop was ignored. Battery charging may also continue from solar or another schedule/controller after a force action is removed.

Command records are kept for 24 hours after a command finishes. There is no fixed completion-time guarantee; `readback_confirmed` is the completion signal.

## 5. JavaScript: browser or Node.js

The browser address bar cannot add the Bearer header. For a local test, open HEM in your browser, open **Developer Tools → Console**, and use `fetch`. Only paste code you understand into the console.

This helper works in modern browsers and Node.js 18+, generates a fresh `Idempotency-Key` per action, and checks HTTP errors without retrying writes:

```js
const HEM_API = 'http://localhost:7338';
const HEM_KEY = 'replace-with-your-key';

const newIdempotencyKey = () => crypto.randomUUID();

async function hemRequest(path, { method = 'GET', body, idempotencyKey } = {}) {
  const response = await fetch(`${HEM_API}${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${HEM_KEY}`,
      ...(idempotencyKey ? { 'Idempotency-Key': idempotencyKey } : {}),
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(10000),
  });
  const result = await response.json();
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}: ${result.error ?? 'Request failed'}`);
  }
  return result;
}

const status = await hemRequest('/api/control/status');
console.log(status.summary, status);
if (!status.ok) console.warn('Current inverter status is unavailable');

// Uncomment ONLY the action you intend to perform. To retry a timed-out
// command safely, reuse the SAME idempotency key.
// const key = newIdempotencyKey();
// const result = await hemRequest('/api/control/force-charge', {
//   method: 'POST', body: { minutes: 60 }, idempotencyKey: key,
// });
// console.log('command', result.command_id);
// const command = await hemRequest(`/api/commands/${result.command_id}`);
// console.log('state', command.data.state); // queued → dispatched → readback_confirmed
// await hemRequest('/api/control/force-charge/stop', { method: 'POST', idempotencyKey: newIdempotencyKey() });
```

In Node.js, save as `.mjs` to use top-level `await`; prefer reading the key from an environment variable in real scripts. In a browser, HTTPS pages may block HTTP requests as mixed content; private-network policies can also apply. Use curl or a suitably configured HTTPS API address rather than disabling browser security.

Do not embed a real key in public frontend code: anyone who can read that code can use the key with its enabled permissions.

## 6. Python: standard library only

Set environment variables before running the script:

```bash
export HEM_API='http://localhost:7338'
export HEM_KEY='replace-with-your-key'
```

```python
import json
import os
import uuid
from urllib.error import HTTPError
from urllib.request import Request, urlopen

BASE_URL = os.environ.get("HEM_API", "http://localhost:7338").rstrip("/")
API_KEY = os.environ["HEM_KEY"]


def hem_request(path, method="GET", body=None, idempotency_key=None):
    headers = {"Authorization": f"Bearer {API_KEY}"}
    if idempotency_key is not None:
        # Retries of the same command must reuse the SAME key.
        headers["Idempotency-Key"] = idempotency_key
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    request = Request(BASE_URL + path, data=data, headers=headers, method=method)
    try:
        with urlopen(request, timeout=10) as response:
            return json.load(response)
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"HTTP {error.code}: {detail}") from error


status = hem_request("/api/control/status")
print(status["summary"])
if not status["ok"]:
    print("Current inverter status is unavailable")

# Uncomment ONLY the action you intend to perform. To retry a timed-out
# command safely, reuse the SAME idempotency key.
# key = str(uuid.uuid4())
# result = hem_request("/api/control/force-charge", "POST", {"minutes": 60}, key)
# command = hem_request(f"/api/commands/{result['command_id']}")
# print(command["data"]["state"])  # queued -> dispatched -> readback_confirmed
# hem_request("/api/control/force-charge/stop", "POST", None, str(uuid.uuid4()))
```

Network failures and timeouts propagate to the caller. A timeout does not prove that an action was rejected: it might already have been queued. Retry with the **same** idempotency key and poll the command state — that is the safe way to find out what happened.

## 7. Troubleshooting

| Symptom | Meaning and next step |
|---|---|
| Connection refused | Verify HEM is running, the API key/port are configured, and the listener is applied. On older installs, starting the API the first time still needs an app restart; port/listen-address changes after that apply immediately. Check host, configured port, firewall and container port mapping. |
| HTTP 401 | Key missing or incorrect. Send `Authorization: Bearer <key>` on every request. A running server uses saved key changes for subsequent requests. |
| HTTP 403, `External battery control is disabled` | Authentication succeeded, but start permission is off. Stops for an action you started earlier still work (recovery); everything else needs the toggle. Working GET status is not evidence of write permission. |
| HTTP 400 on start | Check JSON, content type, the `Idempotency-Key` header (16–128 characters, no whitespace) and integer duration 1–1439. Also read the error: the existing Quick Action handler can refuse an action, for example an opposite-direction conflict. |
| HTTP 409, `in progress` / `already used with a different request` | Idempotency: the first means the same key is already running — poll its `command_id`; the second means the key was reused with a different payload — generate a fresh key. |
| HTTP 409 on start, otherwise | No inverter snapshot is available yet. Wait for HEM to connect and obtain a reading. |
| HTTP 429 with `Retry-After` | Rate limit hit (failed sign-ins, reads or starts). Wait for the interval in the header and retry; legitimate clients should back off, not hammer. |
| HTTP 200 with status `ok:false` | The status endpoint is reachable, but current inverter state is unavailable. Check connection/freshness fields; this is different from HTTP request failure. |
| Stop returns success but activity looks unchanged | Allow time for writes and newer readback. Poll the stop's `command_id` for `readback_confirmed`, and check mode, schedules, other controllers and conditions; activity alone does not identify its cause. |
| Command state stays `unknown` | HEM was restarted (or lost its inverter link) while the command was in flight. It will not re-arm on its own — inspect the inverter in the app and act again if needed. |
| Browser request fails but curl works | Check mixed-content/private-network restrictions, the browser-origin allow-list, and use the correct host from the browser's machine. |
| Other HTTP errors | Check path/method and any reverse proxy. The two GET reads, four mutations and the command-status endpoint are the supported authenticated surface; main-server settings routes are not available here. |

## 8. Retry rules and limitations

These endpoints deliberately reuse the app's Quick Actions; they do not introduce a separate restore system.

- **Retries are safe only with the same `Idempotency-Key` and identical payload** — HEM replays the original response and never queues a second command. A retry with a new key is a *new* command and can replace the original restore point. Generate a fresh key for each new action.
- Stop restores according to the existing Quick Action logic, not a universal reset-to-Eco policy. Stop Charge can restore a pre-action non-Eco mode.
- At Force Charge window expiry, the previous charge schedule is not automatically restored. The configured slot remains until changed and may matter on a later day.
- Force Discharge retains the existing poll-loop auto-restoration behaviour at expiry; HEM must remain running and communicate with the inverter for restoration writes.
- Command tracking survives a HEM restart, but a command that was mid-flight when HEM stopped is marked `unknown`: HEM never re-arms an action on its own, and it is your signal to inspect the inverter.
- A POST acknowledgement is acceptance, not inverter confirmation — that is exactly what `command_id` states distinguish. Neither a lost HTTP response nor missing Quick Action metadata is enough to decide safely that no action occurred; retry with the same key and check the command state.
- The external API is not an emergency-stop mechanism. Use the inverter's appropriate physical controls and manufacturer guidance for emergencies.

For general installation and networking, see [INSTALL.md](INSTALL.md) and the [README](README.md).

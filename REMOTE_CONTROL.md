# Remote Battery Control API

Use Home Energy Manager's authenticated API to read battery status and trigger the same Quick Actions available in the app. Requests go to HEM, which communicates with the inverter; your script does not connect directly to Modbus.

**Start requests change real battery operation immediately.** Run the examples individually, not as one script. This API is not a future scheduler, and a successful request is not confirmation that the inverter has finished applying it.

## 1. Enable the API

1. Open **Settings → Remote / Mobile Network Access → Authenticated API**.
2. Set a long, random API key and choose a port. The default authenticated API port is **7338**.
3. Save and restart HEM to start the separate API server. Port changes also require a restart.
4. For charging/discharging commands, enable **Allow battery control through the authenticated API**. This toggle saves immediately; no restart is needed.

Reading status requires a valid key but **does not require battery-control permission**. A working status request does not prove that write permission is enabled. All four actions, including both Stop actions, require it.

Disabling write permission prevents subsequent external actions; it does not cancel an action already accepted. You can still use the app's Quick Actions buttons.

### Address and security

- On the machine running HEM: `http://localhost:7338`.
- From another device: use the HEM machine's LAN or VPN address, for example `http://192.168.1.100:7338`.
- `localhost` always means the machine executing the request, not necessarily the machine running HEM. Containers also have their own network context.
- Use your configured port if it differs from 7338. The main dashboard/API normally uses **7337**; these examples target the separate authenticated server.
- Every request needs `Authorization: Bearer <your-key>`. Do not put the key in a URL or query string.
- Plain HTTP does not encrypt the key. Use a trusted network, VPN such as Tailscale, or a TLS-terminating reverse proxy. Do not directly expose this HTTP port to the public internet.
- Use a strong key, not the demonstration value `TEST`. Keep keys out of source control, screenshots, public webpages and shared logs.

The separate API does not expose settings or WebSocket endpoints. Enabling it does not change access to the main HEM server.

## 2. Endpoint reference

All paths below are relative to your authenticated server address.

| Method | Path | Request body | Battery-control permission |
|---|---|---|---|
| GET | `/api/control/status` | None | Not required |
| GET | `/api/snapshot` | None | Not required |
| POST | `/api/control/force-charge` | `{"minutes":60}` | Required |
| POST | `/api/control/force-charge/stop` | None | Required |
| POST | `/api/control/force-discharge` | `{"minutes":30}` | Required |
| POST | `/api/control/force-discharge/stop` | None | Required |

For starts, send `Content-Type: application/json` and **only** an integer `minutes` field, from **1 to 1439 inclusive**. Missing, duplicate or unknown fields, fractional numbers, strings, zero and out-of-range durations are rejected. There are no future start times, absolute end times, power or target-SOC overrides in this request format.

Stops use POST with no body. Opening a Stop URL in the browser address bar sends GET, not POST, and will not stop an action.

## 3. Curl examples

These examples use Bash or a compatible shell. Set these variables once in your terminal:

```bash
HEM_API='http://localhost:7338'
HEM_KEY='replace-with-your-key'
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

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"minutes":60}' \
  "$HEM_API/api/control/force-charge"
```

### Stop Force Charge

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  "$HEM_API/api/control/force-charge/stop"
```

An example acknowledgement is:

```json
{"message":"Force charge stopped","ok":true}
```

This acknowledges the handler's work; queued inverter writes and the next status reading can follow later.

### Start Force Discharge for 30 minutes

Force Discharge is a forced-export Quick Action, not merely permission for the battery to supply the house. Stop an existing Force Charge before starting Force Discharge, and vice versa.

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"minutes":30}' \
  "$HEM_API/api/control/force-discharge"
```

### Stop Force Discharge

```bash
curl --silent --show-error --fail-with-body --max-time 10 \
  -X POST -H "Authorization: Bearer $HEM_KEY" \
  "$HEM_API/api/control/force-discharge/stop"
```

### Read the full inverter snapshot

Use this when you need detailed inverter measurements rather than the battery operating summary:

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
2. Submit the intended action **once** and inspect its HTTP response.
3. Poll status at a modest interval, such as every 5–10 seconds, with a bounded overall timeout. Faster HTTP polling does not speed up Modbus.
4. Wait for `observed_at` to advance. For starts, `quick_action.phase` is `pending` until newer matching readback is seen, then `active`; it becomes `expired` after its recorded deadline.
5. After Stop, inspect newer inverter readings and the restored mode/activity. `quick_action:null` alone is not proof that all queued restore writes have completed.

For example, stopping Force Charge may first acknowledge `Force charge stopped`, then show `Eco — discharging; timed charge armed` after readback. The armed schedule does not by itself mean Stop was ignored. Battery charging may also continue from solar or another schedule/controller after a force action is removed.

There is no fixed completion-time guarantee or per-request completion ID. If readback does not match expectations, inspect HEM's UI/logs rather than repeatedly sending the command.

## 5. JavaScript: browser or Node.js

The browser address bar cannot add the Bearer header. For a local test, open HEM in your browser, open **Developer Tools → Console**, and use `fetch`. Only paste code you understand into the console.

This helper works in modern browsers and Node.js 18+ and checks HTTP errors without retrying writes:

```js
const HEM_API = 'http://localhost:7338';
const HEM_KEY = 'replace-with-your-key';

async function hemRequest(path, { method = 'GET', body } = {}) {
  const response = await fetch(`${HEM_API}${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${HEM_KEY}`,
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

// Uncomment ONLY the action you intend to perform:
// await hemRequest('/api/control/force-charge', {
//   method: 'POST', body: { minutes: 60 },
// });
// await hemRequest('/api/control/force-charge/stop', { method: 'POST' });
// await hemRequest('/api/control/force-discharge', {
//   method: 'POST', body: { minutes: 30 },
// });
// await hemRequest('/api/control/force-discharge/stop', { method: 'POST' });
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
from urllib.error import HTTPError
from urllib.request import Request, urlopen

BASE_URL = os.environ.get("HEM_API", "http://localhost:7338").rstrip("/")
API_KEY = os.environ["HEM_KEY"]


def hem_request(path, method="GET", body=None):
    headers = {"Authorization": f"Bearer {API_KEY}"}
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

# Uncomment ONLY the action you intend to perform:
# hem_request("/api/control/force-charge", "POST", {"minutes": 60})
# hem_request("/api/control/force-charge/stop", "POST")
# hem_request("/api/control/force-discharge", "POST", {"minutes": 30})
# hem_request("/api/control/force-discharge/stop", "POST")
```

Network failures and timeouts propagate to the caller. A timeout does not prove that an action was rejected: it might already have been queued. Check status before deciding what to do next.

## 7. Troubleshooting

| Symptom | Meaning and next step |
|---|---|
| Connection refused | Verify HEM is running, the API key/port have been saved, and HEM was restarted. Check host, configured port, firewall and container port mapping. |
| HTTP 401 | Key missing or incorrect. Send `Authorization: Bearer <key>` on every request. A running server uses saved key changes for subsequent requests. |
| HTTP 403, `External battery control is disabled` | Authentication succeeded, but write permission is off. Enable the separate battery-control toggle. Working GET status is not evidence of write permission. |
| HTTP 400 on start | Check JSON, content type and integer duration 1–1439. Also read the error: the existing Quick Action handler can refuse an action, for example an opposite-direction conflict. |
| HTTP 409 on start | No inverter snapshot is available yet. Wait for HEM to connect and obtain a reading. |
| HTTP 200 with status `ok:false` | The status endpoint is reachable, but current inverter state is unavailable. Check connection/freshness fields; this is different from HTTP request failure. |
| Stop returns success but activity looks unchanged | Allow time for writes and newer readback. Check mode, schedules, other controllers and conditions; activity alone does not identify its cause. Inspect logs if the expected change never appears. |
| Browser request fails but curl works | Check mixed-content/private-network restrictions and use the correct host from the browser's machine. |
| Other HTTP errors | Check path/method and any reverse proxy. These six endpoints are the supported authenticated surface; main-server settings routes are not available here. |

## 8. Restoration and retry limitations

These endpoints deliberately reuse the app's Quick Actions; they do not introduce a separate restore system.

- Repeating a start resets its duration and can replace the original restore point. **Do not automatically retry start requests.**
- Stop restores according to the existing Quick Action logic, not a universal reset-to-Eco policy. Stop Charge can restore a pre-action non-Eco mode.
- At Force Charge window expiry, the previous charge schedule is not automatically restored. The configured slot remains until changed and may matter on a later day.
- Force Discharge retains the existing poll-loop auto-restoration behaviour at expiry; HEM must remain running and communicate with the inverter for restoration writes.
- Restore/timing state does not survive a HEM restart. A repeated Stop cannot reliably replay failed restoration writes.
- A POST acknowledgement is acceptance, not inverter confirmation. Neither a lost HTTP response nor missing Quick Action metadata is enough to decide safely that no action occurred.
- The external API is not an emergency-stop mechanism. Use the inverter's appropriate physical controls and manufacturer guidance for emergencies.

For general installation and networking, see [INSTALL.md](INSTALL.md) and the [README](README.md).

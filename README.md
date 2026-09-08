# Home Energy Manager

**Monitor and control your GivEnergy solar and battery system from your own computer — no cloud account needed.**

Home Energy Manager connects directly to your inverter over your home network and shows you live data in real time. You can check your solar generation, battery charge, and energy costs, set charge schedules, and automate your battery — all without sending any data to the internet.

> 💬 **Got a question or hit a snag?** Try asking your favourite AI assistant first — just point it at the [**installation guide**](https://github.com/psylsph/home-energy-manager/blob/master/INSTALL.md) and ask your question. If you don't get your answer, feel free to [raise an issue](https://github.com/psylsph/home-energy-manager/issues).

<div align="center">

<a href="https://www.buymeacoffee.com/psylsph" target="_blank"><img src="https://cdn.buymeacoffee.com/buttons/v2/default-blue.png" alt="Buy Me a Coffee" style="height: 80px !important;width: 275px !important;" ></a>

</div>

## Screenshots

<table>
  <tr>
    <td align="center"><b>Status Dashboard</b><br><img src=".github/screenshots/status.png" width="400"></td>
    <td align="center"><b>Status — Mobile</b><br><img src=".github/screenshots/status-mobile.png" width="200"></td>
  </tr>
  <tr>
    <td align="center"><b>Power Chart</b><br><img src=".github/screenshots/power.png" width="400"></td>
    <td align="center"><b>Battery Detail</b><br><img src=".github/screenshots/battery.png" width="400"></td>
  </tr>
    <tr>
    <td align="center"><b>Inverter Info</b><br><img src=".github/screenshots/inverter.png" width="400"></td>
    <td align="center"><b>Meters</b><br><img src=".github/screenshots/meters.png" width="400"></td>
  </tr>

  <tr>
    <td align="center"><b>Energy History</b><br><img src=".github/screenshots/history.png" width="400"></td>
    <td align="center"><b>History — Solar</b><br><img src=".github/screenshots/history-solar.png" width="400"></td>
  </tr>
  <tr>
    <td align="center"><b>History — Home</b><br><img src=".github/screenshots/history-home.png" width="400"></td>
    <td align="center"><b>Forecast</b><br><img src=".github/screenshots/forecast.png" width="290"></td>
</tr>

  <tr>
    <td align="center"><b>Control Panel</b><br><img src=".github/screenshots/control.png" width="400"></td>
    <td align="center"><b>Settings</b><br><img src=".github/screenshots/settings.png" width="400"></td>
  </tr>
  <tr>
    <td align="center"><b>Developer Console</b><br><img src=".github/screenshots/developer-mode.png" width="400"></td>
    <td align="center"><b>Consumption Reports</b><br><img src=".github/screenshots/power-reports-1.png" width="400"></td>
  </tr>
  <tr>
    <td align="center"><b>Consumption Report (PDF)</b><br><img src=".github/screenshots/power-reports-2.png" width="400"></td>
    <td></td>
  </tr>
</table>

---

## 🚀 Getting Started

### 1. Download and install

Go to the [**Releases page**](https://github.com/psylsph/home-energy-manager/releases/latest) and download the file for your system:

| Your computer | Look for the file name containing |
|---|---|
| 🪟 **Windows** | `Windows-MSI-...msi` |
| 🍎 **Mac with Apple Silicon** (M1/M2/M3/M4) | `macOS-Apple-Silicon-...dmg` |
| 🍎 **Mac with Intel processor** | `macOS-Intel-...dmg` |
| 🐧 **Linux x86_64** (Ubuntu, Debian, etc.) | `Linux-Debian-x86_64-...deb` |
| 🐧 **Linux x86_64** (Fedora, openSUSE, etc.) | `Linux-RPM-x86_64-...rpm` |
| 🍓 **Raspberry Pi / ARM64 Linux** (Ubuntu, Debian) | `Linux-Debian-ARM64-...deb` |
| 🍓 **Raspberry Pi / ARM64 Linux** (Fedora, openSUSE) | `Linux-RPM-ARM64-...rpm` |

**Windows users** — Windows SmartScreen may show "Windows protected your PC" because the app is not code-signed. This is open-source software you can inspect on GitHub, and the installer is scanned clean by VirusTotal. If your antivirus flags it as malware, please report a security vulnerability at <https://github.com/psylsph/home-energy-manager/issues>.

To run the MSI if SmartScreen appears:

1. Click **"More info"** on the SmartScreen screen
2. Click **"Run anyway"**

If the installer itself won't open, right-click the `.msi` → **Properties** → check the **"Unblock"** box → **OK**, then run it again. The `Windows-Store-MSIX-...msix` asset is an unsigned Microsoft Store submission package and is not intended for direct installation.

**Mac users** — after opening the `.dmg`, drag the app to your **Desktop** or **Home folder** (not `/Applications`). On first launch, right-click the app → **Open** → **Open** to bypass Gatekeeper. See the [FAQ](./FAQ.md) if you get stuck.

**Linux users** — packaged installs may need two runtime libraries, while
building desktop bundles from source requires additional Linux development
packages. See [INSTALL.md](./INSTALL.md#linux-system-requirements) for details.

**Proxmox users** — a first-party helper can create an unprivileged Debian 13
LXC and run Home Energy Manager headlessly without Docker. See the
[Proxmox VE LXC instructions](./INSTALL.md#proxmox-ve-lxc).

### 2. Find your inverter's IP address

The app needs the IP address of the small WiFi or Ethernet dongle connected to your inverter. You can find this in your router's device list — look for a device named "GivEnergy" or check the MAC address printed on the dongle.

Not sure? Don't worry — the app can find it for you (see step 4 below).

### 3. Connect

1. Open the app and go to **Settings** (the ⚙️ icon at the bottom)
2. Enter your inverter's IP address in the **Host** field
3. Click **Connect**

Live data should appear on the Status page within a few seconds. The serial number is detected automatically.

### 4. Can't find the IP? Let the app scan for you

Click **Scan Network** on the Settings page. The app will search your local network for GivEnergy data adapters and list any it finds. Click on one to auto-fill the IP address.

> **Tip**: If the connection keeps dropping or data looks wrong, try a wired Ethernet connection between your data adapter and router. The WiFi dongles can be unreliable.

---

## Features

Home Energy Manager connects directly to your inverter over your home network. It never sends data to the internet and doesn't need a GivEnergy Cloud account.

### Monitoring

- **Real-time dashboard** — see solar generation, battery charge level, grid import/export, and home consumption updating live
- **Energy flow diagram** — animated visual showing where your power is flowing right now (solar → battery → home → grid)
- **Power chart** — live chart tracking solar, battery, grid, and home power with selectable time ranges from 15 minutes to 7 days. Click legend labels to show or hide individual lines.
- **Battery detail** — individual cell voltages, temperatures, and health per battery module
- **Solar page** — voltage, current, and power for each solar string (supports dual-string systems)
- **Inverter page** — model name, firmware versions, serial number, temperatures, and all electrical readings at a glance
- **Meters page** — external meter readings with per-phase voltage, current, and power, plus a CT clamp status card
- **Cold battery warning** — alerts you when your battery temperature drops near freezing so you can protect it

### History & Cost Tracking

- **Time-range charts** — 7 selectable ranges from 15 minutes to 7 days, covering solar, battery, grid, and home energy
- **Energy breakdown views** — separate charts for solar, home, grid, and battery with shared time-range selection
- **Month calendar view** — daily energy totals at a glance for the whole month
- **Cost tracking** — enter your import and export tariffs to see running cost estimates on your charts
- **CSV export** — download your energy history as a spreadsheet
- **PDF consumption reports** — generate formatted reports with charts and summary tables for solar, home, grid, and battery energy, including cost breakdowns. Open **Consumption Report** from the Power page, then choose **Print / save as PDF**. The History page exports CSV only.
- **Consumption Reports** — summary statistics for any time range including total energy, peak power, solar coverage percentage, and grid dependency, with time-bucketed breakdowns exportable as CSV

### Forecasting & Planning

- **Solar forecast** — a 48-hour generation forecast built from live weather data (Open-Meteo — free, no account or API key needed) and automatically calibrated against your own generation history, so it learns your panels' real-world performance over the first couple of weeks
- **Consumption profile** — your household's typical hourly electricity usage, learned from your own history and shown with typical low/high ranges
- **Battery projection & overnight charge plan** — see where your battery charge is heading hour by hour, and get a recommended overnight grid charge when it would otherwise dip below your minimum level. The plan is sized to the smallest charge that holds your floor and placed in your cheapest import tariff window; one click applies it to the inverter

### Control

- **Charge & discharge schedules** — set time slots for when your battery charges from the grid or discharges to power your home (up to **10 slots** on supported models)
- **Battery modes** — switch between Eco (automatic self-consumption), Timed Discharge, and Pause Discharge, which holds stored energy while still allowing solar or scheduled grid charging
- **Force Charge / Force Discharge** — mutually exclusive manual controls with live start/stop confirmation; stop the active action before starting the opposite one
- **SOC control** — adjust battery reserve level, charge/discharge power limits, and charge target
- **Battery calibration** — start calibration from the app when your battery needs it (auto-detected)
- **Load Discharge Limiter** — pause battery discharge during high-demand periods with a configurable power threshold and time window
- **Inverter Temperature Limiter** — pause discharge at a configurable heatsink temperature and restore normal Eco after the inverter cools

### Automation

- **Octopus Cosy** (beta) — enter your three Cosy cheap-rate windows and the app automatically charges your battery during each one, switching back to Eco mode in between. Survives an app restart mid-slot.
- **Octopus Agile** (beta) — enter your postcode and price thresholds. The app charges when Agile prices are low, discharges when they're high, and stays in Eco the rest of the time. Includes a live 24-hour price forecast grid with daily savings estimates.
- **Auto Winter Mode** — protects your battery from cold by automatically charging it from the grid when the temperature drops. You set the temperature threshold and target charge level. Works the same way as GivEnergy Cloud's winter mode, but runs entirely on your own machine.
- **Alert notifications (Telegram, ntfy & Pushover)** — set thresholds for battery temperature, inverter temperature, SOC, solar clipping, grid offline, and battery over-temp. Get an alert message when something's wrong — and another when it's back to normal. Send `/status`, `/today`, or `/report` in your Telegram chat for a live system snapshot, today's cost summary, or yesterday's full consumption report. Choose between a Telegram bot, ntfy or Pushover push notifications, or any combination.
- **Octopus smart-meter dashboard** — add your Octopus account number and API key to see supplier-recorded electricity import, export, and gas usage alongside your inverter data, including billing costs, monthly and yearly summaries, and a comparison against the app's own readings. Export as CSV or PDF. The Octopus tab stays hidden until configured.

### Compatibility

- **Works with every GivEnergy inverter model** — Gen 1, Gen 2, Gen 3, Gen 4, Three Phase, AC Three Phase, HV Gen 3, All-in-One, AIO Hybrid, AIO Commercial, and **Gateway**
- **Three-phase and commercial systems** — fully supported, including the GIV-3HY family and All-in-One units
- **Smart meter detection** — handles LoRA-linked meters and slow-responding CT clamps so nothing gets missed at startup

---

## Supported Inverters

Home Energy Manager works with all known GivEnergy inverter models. Real-time monitoring, Force Charge/Discharge, Cosy and Agile automation, and Auto Winter Mode work on every model. The main difference between models is how many charge/discharge schedule slots you can set:

### 10-slot schedules ✅

*Full control — live data, up to 10 charge + 10 discharge slots, all limits and modes*

| Model | Notes |
|---|---|
| **Gen 3 Hybrid** (5kW/8kW/10kW) | Most common. Extended 10-slot schedules require ARM firmware ≥ 303. |
| **Gen 4 Hybrid** | Latest generation |
| **Three Phase** (e.g. GIV-3HY-11 11kW) | Full three-phase support |
| **AC Three Phase** | AC-coupled three-phase |
| **HV Gen 3** | High-voltage hybrid |
| **All-in-One** (3.6kW/5kW/6kW) | Commercial all-in-one units |
| **All-in-One Hybrid** | Combined hybrid + AIO |
| **AIO Commercial** | Commercial three-phase variant |
| **Gateway** *(experimental)* | System controller / AC hub for 1–3 AIO units. Full schedule, mode, and rate-limit control via the three-phase register set. |

### 2-slot schedules ✅

*Full control with the simpler 2-slot layout*

| Model | Notes |
|---|---|
| **Gen 2 Hybrid** | Standard home hybrid inverter |
| **Gen 3 Plus Hybrid** / **Polar Hybrid** | Newer single-phase variants |
| **PV Inverter** (no battery) | Solar-only — battery controls are hidden |

### 1-slot schedules ✅

*Live data, power limits, SOC, and modes — but only one charge + one discharge slot*

| Model | Notes |
|---|---|
| **Gen 1 Hybrid** | Older generation |
| **AC Coupled** (standard & Mk2) | Retrofit battery system |

> **Not sure which model you have?** Just connect the app to your inverter and check the Inverter tab — it shows the detected model name and details automatically.

---

## Supported Platforms

| Platform | Available as |
|---|---|
| Windows | `.msi` installer |
| macOS (Apple Silicon & Intel) | `.dmg` |
| Linux (x86_64) | `.deb` and `.rpm` packages |
| Raspberry Pi (64-bit OS) | `.deb` (ARM64) |
| Any device with a browser | Access the web UI at `http://your-pi-ip:7337` when running headless |

The app also runs as a **headless server** — a background service with no window, serving the full UI to any browser on your network. Great for Raspberry Pi or an always-on server. See [INSTALL.md](./INSTALL.md) for setup instructions.

External software can use an authenticated integration API for battery status and optional Quick Actions — see [Authenticated API](#-authenticated-api-integration) below. Configure its key, separate port and default-off battery-control permission under **Settings → Remote / Mobile Network Access** (Developer Mode not required).

---

## 🔌 Authenticated API Integration

A separate HTTP server (default port **7338**, Bearer-token authenticated) lets external software read inverter data and — with explicit permission — use the same four battery Quick Actions as the app's buttons. The main dashboard server is unchanged by any of this.

### Setup

1. **Settings → Remote / Mobile Network Access → Authenticated API**: set an API key (long and random) and port, then save and restart HEM to start the server.
2. Reading data needs only the key. To allow battery writes, toggle on **Allow battery control through the authenticated API** — it applies immediately (off by default, including after upgrades; disabling does not stop an action already accepted).
3. Bearer tokens are not encrypted over plain HTTP — use a trusted network, VPN, or TLS-terminating reverse proxy.

### Endpoints

| Method | Path | Body | Write permission |
|---|---|---|---|
| GET | `/api/snapshot` | None | No |
| GET | `/api/control/status` | None | No |
| POST | `/api/control/force-charge` | `{"minutes":60}` | Yes |
| POST | `/api/control/force-charge/stop` | None | Yes |
| POST | `/api/control/force-discharge` | `{"minutes":60}` | Yes |
| POST | `/api/control/force-discharge/stop` | None | Yes |

Starts act immediately (duration 1–1439 minutes; extra fields are rejected). Stop requests need no body. The actions reuse the Quick Action handlers exactly: same model-aware registers, restore behaviour, configured power limits, and mutual exclusion (stop one direction before starting the other). A `200` response means **accepted and queued**, not confirmed by the inverter. Other errors: `400` invalid input or refused action, `401` bad key, `403` control disabled, `409` no inverter snapshot yet.

```bash
HEM_API='http://192.168.1.100:7338'
HEM_KEY='replace-with-your-key'

curl --fail-with-body "$HEM_API/api/control/force-charge" \
  -H "Authorization: Bearer $HEM_KEY" -H 'Content-Type: application/json' \
  --data '{"minutes":60}'

curl --fail-with-body -X POST "$HEM_API/api/control/force-charge/stop" \
  -H "Authorization: Bearer $HEM_KEY"

curl --fail-with-body "$HEM_API/api/control/force-discharge" \
  -H "Authorization: Bearer $HEM_KEY" -H 'Content-Type: application/json' \
  --data '{"minutes":30}'

curl --fail-with-body -X POST "$HEM_API/api/control/force-discharge/stop" \
  -H "Authorization: Bearer $HEM_KEY"

curl --fail-with-body "$HEM_API/api/control/status" \
  -H "Authorization: Bearer $HEM_KEY"
```

### Summary status

`GET /api/control/status` reports cached state only (no Modbus reads, no writes; `Cache-Control: no-store`). Mode, measured activity, controlling automation, schedules and restrictions are reported independently because they coexist — a force-charge window can be active while the battery is idle, and discharging is not necessarily grid export:

```json
{
  "ok": true,
  "summary": "Force Charge — charging; 58 minutes remaining",
  "mode": "eco",
  "activity": "charging",
  "control_source": "force_charge",
  "control_phase": "active",
  "remaining_minutes": 58,
  "schedules": {"charge": "active", "export": "off", "demand_discharge": "off"},
  "conditions": [],
  "connection": "connected",
  "stale": false,
  "observed_at": "2027-01-15T12:00:00+00:00"
}
```

- `summary` is for display; integrate against the structured fields (wording may change).
- `mode`: `eco`, `eco_paused`, `timed_demand`, `timed_export`, `export_paused`, `unknown`. `activity`: `charging`, `discharging`, `idle`, `unavailable` — observed operation, not requested action or inferred cause.
- `control_source` / `control_phase`: HEM's best-known controller (Quick Action, safety limiter, Timed Export, Cosy/Agile/Adaptive/winter automation, or the inverter's own schedule) and its phase. A Quick Action reads `pending` until a newer snapshot confirms it, then `active` for the window duration even if the battery is idle; `expired` when the recorded deadline passes.
- `remaining_minutes`: time left in a known Quick Action window (rounded up), otherwise `null` — the window, not time-to-full.
- `schedules`: each of charge / export / demand-discharge is `off`, `armed` (outside its window), `active`, or `unknown`, evaluated on the inverter's clock. A HEM-managed export schedule stays `armed` outside windows even when physical slots are temporarily cleared.
- `automation`, `calibration`, `maintenance`, `limits`: configuration and phase detail for Cosy, Agile, Adaptive Charge, winter automation, managed Timed Export, battery calibration/maintenance, and configured SOC/rate limits.
- `conditions`: all simultaneous faults, protections, pauses and unknown states as `{code, label}` — never collapsed into one label.
- Readings older than three poll intervals (minimum 60s), implausibly future-dated readings, and disconnected/reconnecting states set `stale`/`ok: false` and refuse to present a current mode or activity; `conditions` is then not a health statement.

### Known limitations (shared with the buttons)

- Repeating a start resets its duration and can replace the original restore point — don't build blind automatic retries.
- Timed charging does not auto-restore the previous schedule when its window ends; the slot stays configured until changed.
- Restore state doesn't survive a HEM restart; a repeated Stop can't always replay failed restore writes.
- Stop restores per existing Quick Action logic (Stop Charge may restore a pre-action non-Eco mode); it is not a separate integration-specific default.

---

## 📱 Using on Your Phone Away From Home

Home Energy Manager has a built-in web server, so you can access it from your phone's browser. Combined with [**Tailscale**](https://tailscale.com) (a free, zero-config VPN), you can check your system from anywhere — no cloud dependency, no port forwarding, no static IP.

### Setup

1. **Install Tailscale** on the machine running Home Energy Manager and on your phone
2. Both devices join the same Tailscale network
3. Open your phone browser to `http://<tailscale-ip>:7337`
4. Tap **Share → Add to Home Screen** for an app-like icon

Tailscale encrypts everything end-to-end, so your inverter data stays private.

<details>
<summary><b>📋 Detailed instructions</b></summary>

Install Tailscale on your server machine:

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
# Note the Tailscale IP shown (or find it later with: tailscale ip -4)
```

Then on your phone:

1. Install the **Tailscale** app from the App Store / Play Store
2. Log in to the same account — your devices appear automatically
3. Open Safari / Chrome and go to `http://<tailscale-ip>:7337`
4. Tap **Share → Add to Home Screen** for a native-app-like experience

> 💡 **Tip**: Set Home Energy Manager to run on boot so it's always available.

</details>

<details>
<summary><b>🔧 Alternative: Tailscale Funnel (no app needed on phone)</b></summary>

If you don't want Tailscale on your phone, you can expose the web UI via a public `.ts.net` URL:

```bash
sudo tailscale serve --bg --https 443 127.0.0.1:7337
sudo tailscale funnel --bg 443
```

Your app will be available at `https://<machine-name>.<tailnet-name>.ts.net`. Tailscale handles HTTPS. Note: Funnel is a paid Tailscale feature.

</details>

### Compared to the cloud portal

| | Cloud portal | Home Energy Manager + Tailscale |
|---|---|---|
| **Speed** | 1–3 second delay | Real-time |
| **Internet needed?** | Always | Only when away from home |
| **Cloud dependency** | Depends on GivEnergy servers | None |
| **Privacy** | Data via GivEnergy | End-to-end encrypted |
| **Cost** | Free | Free |

---

## About the Name Change

This project was originally called **GivEnergy-Local**. The user-facing name is now **Home Energy Manager**, but the internal executable is still called `givenergy-local` and your settings and history are stored in the same place (`~/.givenergy-local`). Upgrading is seamless — everything carries over.

---

## Credits

🙏 **Huge thanks to the open-source projects that made this possible:**, this project would not exist without the pioneering reverse-engineering work of the GivEnergy open-source community.

- **[GivTCP](https://github.com/GivEnergy/giv_tcp)** — the original GivEnergy Modbus integration for Home Assistant. This app builds on the protocol mapping and write methodology that GivTCP established.

- **[givenergy-modbus](https://github.com/dewet22/givenergy-modbus)** — the definitive Python reference library for the GivEnergy Modbus protocol. Its detailed register map and working reference implementation were invaluable.

Both projects are open-source and available on GitHub. If you find this app useful, consider giving them a star too ⭐

## License

MIT — see [LICENSE](./LICENSE).

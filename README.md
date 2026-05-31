# GivEnergy Local

**Desktop app for monitoring and controlling GivEnergy solar inverters over your local network — no cloud account needed.**

![License](https://img.shields.io/badge/license-MIT-blue.svg)

> 🙏 **Huge thanks to the open-source reverse-engineering efforts that made this possible:**  
> [**GivTCP**](https://github.com/GivEnergy/giv_tcp) — the original GivEnergy Modbus integration for Home Assistant  
> [**givenergy-modbus**](https://github.com/dewet22/givenergy-modbus) — detailed register map, protocol reference, and Python library

## 🚀 Getting Started

### 1. Download and install

Download the latest release for your platform from the [**Releases page**](https://github.com/psylsph/givenergy-local/releases/latest):

| Platform | File |
|---|---|
| 🪟 Windows | `GivEnergy_Local_*_x64-setup.exe` |
| 🍎 macOS (Apple Silicon — M1/M2/M3/M4) | `GivEnergy Local_*_aarch64.dmg` |
| 🍎 macOS (Intel) | `GivEnergy Local_*_x64.dmg` |
| 🐧 Linux | `givenergy-local_*_amd64.deb` |

> **macOS users**: Do NOT drag the app to `/Applications` — macOS blocks unsigned apps there. Drag it to your **Desktop** or **Home folder** instead. On first launch, right-click the app → **Open** → **Open** to bypass Gatekeeper.

### 2. Find your inverter's IP address

You need the IP address of your inverter's data adapter (the small WiFi or Ethernet dongle connected to your inverter). You can find this in your router's device list — look for a device named "GivEnergy" or check the MAC address printed on the dongle.

The adapter listens on **port 8899**.

### 3. Connect

1. Open the app and go to **Settings** (gear icon ⚙️ in the sidebar)
2. Enter your inverter's IP address in the **Host** field
3. Click **Connect**

The app connects to your inverter over your local network. The serial number is detected automatically. Live data should appear on the Status page within a few seconds.

### 4. (Optional) Can't find the IP? Scan your network

Click **Scan Network** on the Settings page. The app will scan your local network for GivEnergy data adapters and list any it finds. Click on one to auto-fill the IP address.

> **Tip**: If the connection keeps dropping or data looks wrong, try a wired Ethernet connection between your data adapter and router. The WiFi dongles can be unreliable.

## Screenshots

<table>
  <tr>
    <td align="center"><b>Status Dashboard</b><br><img src=".github/screenshots/status.png" width="400"></td>
    <td align="center"><b>Energy History</b><br><img src=".github/screenshots/history.png" width="400"></td>
  </tr>
  <tr>
    <td align="center"><b>Battery Detail</b><br><img src=".github/screenshots/battery.png" width="400"></td>
    <td align="center"><b>Control Panel</b><br><img src=".github/screenshots/control.png" width="400"></td>
  </tr>
  <tr>
    <td align="center"><b>Settings</b><br><img src=".github/screenshots/settings.png" width="400"></td>
    <td align="center"><b>Developer Console</b><br><img src=".github/screenshots/developer-mode.png" width="400"></td>
  </tr>
</table>

## What it does

GivEnergy Local connects directly to your inverter's WiFi or Ethernet data adapter over your home network. It shows you what's happening right now and lets you change settings without needing a GivEnergy cloud account or portal login.

- **Real-time dashboard** — see solar generation, battery charge level, grid import/export, and home consumption updating live
- **Energy flow diagram** — visual animation showing where power is flowing right now (solar → battery → home → grid)
- **Battery details** — individual cell voltages, temperatures and health per battery module
- **Charge & discharge schedules** — set time slots for when your battery charges from the grid or discharges to power your home
- **Mode switching** — Eco, Timed Discharge, and Pause modes
- **SOC control** — adjust battery reserve level, charge/discharge power limits, and charge target
- **Auto-discovery** — just enter your inverter's IP address; the serial number is detected automatically
- **History & cost tracking** — 7 time-range charts for solar, battery, grid, and home energy, with configurable import/export tariffs (peak/off-peak)
- **Developer console** — live log viewer for diagnostics (enable in Settings)

---

## How it works

```
┌─────────────┐                ┌──────────────┐              ┌───────────┐
│  This app   │ ◄── network ──► │  Data adapter │ ◄── serial ──► │ Inverter  │
│  (desktop)  │   port 7337     │  (dongle)     │   port 8899  │ + Battery │
└─────────────┘                 └──────────────┘              └───────────┘
```

The app talks to your inverter's data adapter over your local network using the Modbus TCP protocol. It never connects to the internet or sends data anywhere else.

### Battery SOC

The battery state of charge (SOC) shown on the Status and Battery pages comes
from the inverter's own register (IR 59), which is the same value the official
GivEnergy app and GivTCP report. If this register returns 0 (indicating a
corrupted read), the app falls back to a capacity-weighted average calculated
from all connected battery modules using their `remaining_capacity / capacity`
registers.

For multi-battery systems, each module's individual SOC is shown in the
Battery page module cards. The main SOC display reflects the inverter's
aggregate value.

## Tech Stack

Built with [Tauri 2](https://v2.tauri.app/) (Rust + React), Axum, and TypeScript. See [DESIGN.md](./DESIGN.md) for architecture details and the register map.

## Development

```bash
# 1. Install dependencies
npm install
cargo install tauri-cli

# 2. Build the frontend first (creates dist/)
npm run build

# 3. Build and Run the Rust backend
cd src-tauri && cargo tauri dev
```

## Running Headless (Native)

```bash
# 1. Install dependencies
npm install
cargo install tauri-cli

# 2. Build the frontend first (creates dist/)
npm run build

# 3. Build the Rust backend
cd src-tauri && cargo build --release

# 4. Run headless (no GUI window)
nohup ./target/release/givenergy-local --headless > givenergy-local.log 2>&1 &
```

> The frontend (`dist/`) must be built before the Rust binary, otherwise
> the server won't have any UI files to serve. Alternatively, use `--dist`
> to point to an existing build:
> ```bash
> ./target/release/givenergy-local --headless --dist /path/to/dist
> ```

## Running Headless (Docker)

```bash
# Build and start with docker compose
docker compose up -d

# Rebuild after code changes
docker compose build && docker compose up -d
```

**Persistent data** (settings + history DB) lives in `${HOME}/.givenergy-local` and
is mounted into the container at `/root/.givenergy-local`. This survives restarts.

## Running Multiple Instances

You can run multiple copies of the app with separate settings and history
by setting the `GIVENERGY_LOCAL_CONFIG_DIR` environment variable to a
different directory for each instance.

**Linux / macOS:**

```bash
# Default (uses ~/.givenergy-local/)
./givenergy-local

# Second instance with its own config and history
GIVENERGY_LOCAL_CONFIG_DIR=~/givenergy-instance2 ./givenergy-local

# Headless server on a different port
GIVENERGY_LOCAL_CONFIG_DIR=~/givenergy-server ./givenergy-local --headless --port 8080
```

**Windows (PowerShell):**

```powershell
# Default (uses %USERPROFILE%\.givenergy-local\)
.\givenergy-local.exe

# Second instance
$env:GIVENERGY_LOCAL_CONFIG_DIR = "C:\Users\You\givenergy-config-2"
.\givenergy-local.exe
```

**Windows (Command Prompt):**

```cmd
set GIVENERGY_LOCAL_CONFIG_DIR=C:\Users\You\givenergy-config-2
givenergy-local.exe
```

See [DESIGN.md](./DESIGN.md) for full build instructions, testing, and architecture documentation.

## Credits

This project would not exist without the pioneering reverse-engineering work of the GivEnergy open-source community.

- **[GivTCP](https://github.com/GivEnergy/giv_tcp)** — The original GivEnergy Modbus integration for Home Assistant. This project established the core Modbus protocol mapping, register addresses, and write methodology that this app builds on. Without GivTCP, none of this would be possible.

- **[givenergy-modbus](https://github.com/dewet22/givenergy-modbus)** — The definitive Python reference library for the GivEnergy Modbus protocol. Its detailed register map, frame format documentation, and working reference implementation were invaluable in getting the protocol right — especially the write protocol (function code 6, device address 0x11) and the HHMM timeslot encoding.

Both projects are open-source and available on GitHub. If you find this app useful, consider giving them a star too ⭐

## License

MIT — see [LICENSE](./LICENSE).

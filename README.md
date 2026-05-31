# GivEnergy Local

**Desktop app for monitoring and controlling GivEnergy solar inverters over your local network — no cloud account needed.**

![License](https://img.shields.io/badge/license-MIT-blue.svg)

> 🙏 **Huge thanks to the open-source reverse-engineering efforts that made this possible:**  
> [**GivTCP**](https://github.com/GivEnergy/giv_tcp) — the original GivEnergy Modbus integration for Home Assistant  
> [**givenergy-modbus**](https://github.com/dewet22/givenergy-modbus) — detailed register map, protocol reference, and Python library

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
    <td></td>
  </tr>
</table>

## What it does

GivEnergy Local connects directly to your inverter's WiFi or Ethernet data adapter over your home network. It shows you what's happening right now and lets you change settings without needing a GivEnergy cloud account or portal login.

- **Real-time dashboard** — see solar generation, battery charge level, grid import/export, and home consumption updating live
- **Energy flow diagram** — visual animation showing where power is flowing right now (solar → battery → home → grid)
- **Battery details** — individual cell voltages, temperatures and health per battery module
- **Charge & discharge schedules** — set time slots for when your battery charges from the grid or discharges to power your home
- **Mode switching** — Eco, Timed Demand, Timed Export, and Pause modes
- **SOC control** — adjust battery reserve level, charge/discharge power limits, and charge target
- **Auto-discovery** — just enter your inverter's IP address; the serial number is detected automatically
- **History & cost tracking** — 7 time-range charts for solar, battery, grid, and home energy, with configurable import/export tariffs (peak/off-peak)
- **Developer console** — live log viewer for diagnostics (enable in Settings)

## Download

Download the latest release for your platform from the [Releases page](https://github.com/psylsph/givenergy-local/releases/latest):

| Platform | File |
|---|---|
| 🪟 Windows | `GivEnergy_Local_*_x64-setup.exe` |
| 🍎 macOS (Apple Silicon) | `GivEnergy Local_*_aarch64.dmg` |
| 🍎 macOS (Intel) | `GivEnergy Local_*_x64.dmg` |
| 🐧 Linux | `givenergy-local_*_amd64.deb` |

> **Prerequisites**: Your GivEnergy inverter's WiFi/Ethernet data adapter must be connected to your home network. You need its IP address (find it in your router's device list).

## Quick Start

### 1. Install the app

Download the latest release for your platform from the [Releases page](https://github.com/psylsph/givenergy-local/releases/latest) and install it.

> **macOS users**: Do NOT drag the app to `/Applications`. macOS blocks unsigned apps there. Drag it to your **Desktop** or **Home folder** instead. On first launch, right-click the app → Open → Open to bypass Gatekeeper.

### 2. Find your inverter's IP address

You need the IP address of your inverter's data adapter (the small WiFi/Ethernet dongle connected to your inverter). You can find this in your router's device list — look for a device named "GivEnergy" or check the MAC address printed on the dongle.

The adapter listens on **port 8899**.

### 3. Connect

1. Open the app and go to **Settings** (gear icon in the sidebar)
2. Enter your inverter's IP address in the **Host** field
3. Click **Connect**

The app connects to your inverter over your local network. The serial number is detected automatically. Live data should appear on the Status page within a few seconds.

### 4. (Optional) Scan for inverters

If you're not sure of the IP address, click **Scan Network** on the Settings page. The app will scan your local network for GivEnergy data adapters and list any it finds. Click on one to auto-fill the IP address.

> **Tip**: If the connection keeps dropping or data looks wrong, try a wired Ethernet connection between your data adapter and router. The WiFi dongles can be unreliable.

That's it. No accounts, no cloud, no internet required.

## Requirements

- A GivEnergy solar inverter with a WiFi or Ethernet data adapter
- The data adapter must be on your local network (port 8899)
- Windows 10+, macOS 12+, or Linux (Ubuntu 22.04+)

## How it works

```
┌─────────────┐                ┌──────────────┐              ┌───────────┐
│  This app   │ ◄── network ──► │  Data adapter │ ◄── serial ──► │ Inverter  │
│  (desktop)  │   port 7337     │  (dongle)     │   port 8899  │ + Battery │
└─────────────┘                 └──────────────┘              └───────────┘
```

The app talks to your inverter's data adapter over your local network using the Modbus TCP protocol. It never connects to the internet or sends data anywhere else.

## Tech Stack

Built with [Tauri 2](https://v2.tauri.app/) (Rust + React), Axum, and TypeScript. See [DESIGN.md](./DESIGN.md) for architecture details and the register map.

## Development

```bash
npm install
cargo install tauri-cli
cd src-tauri && cargo tauri dev
```

## Build and Running Headless

```bash
npm install
cargo install tauri-cli
cd src-tauri && cargo build --release
nohup ./target/release/givenergy-local --headless > givenergy-local.log 2>&1 &
```

See [DESIGN.md](./DESIGN.md) for full build instructions, testing, and architecture documentation.

## Credits

This project would not exist without the pioneering reverse-engineering work of the GivEnergy open-source community.

- **[GivTCP](https://github.com/GivEnergy/giv_tcp)** — The original GivEnergy Modbus integration for Home Assistant. This project established the core Modbus protocol mapping, register addresses, and write methodology that this app builds on. Without GivTCP, none of this would be possible.

- **[givenergy-modbus](https://github.com/dewet22/givenergy-modbus)** — The definitive Python reference library for the GivEnergy Modbus protocol. Its detailed register map, frame format documentation, and working reference implementation were invaluable in getting the protocol right — especially the write protocol (function code 6, device address 0x11) and the HHMM timeslot encoding.

Both projects are open-source and available on GitHub. If you find this app useful, consider giving them a star too ⭐

## License

MIT — see [LICENSE](./LICENSE).

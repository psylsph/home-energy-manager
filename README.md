# GivEnergy Local

**Desktop app for monitoring and controlling GivEnergy solar inverters over your local network — no cloud account needed.**

![License](https://img.shields.io/badge/license-MIT-blue.svg)

## What it does

GivEnergy Local connects directly to your inverter's WiFi or Ethernet data adapter over your home network. It shows you what's happening right now and lets you change settings without needing a GivEnergy cloud account or portal login.

- **Real-time dashboard** — see solar generation, battery charge level, grid import/export, and home consumption updating live
- **Energy flow diagram** — visual animation showing where power is flowing right now (solar → battery → home → grid)
- **Battery details** — individual cell voltages, temperatures and health per battery module
- **Charge & discharge schedules** — set time slots for when your battery charges from the grid or discharges to power your home
- **Mode switching** — Eco, Timed Demand, Timed Export, and Pause modes
- **SOC control** — adjust battery reserve level, charge/discharge power limits, and charge target
- **Auto-discovery** — just enter your inverter's IP address; the serial number is detected automatically

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

1. Download and install the app for your platform
2. Enter your inverter's IP address on the Settings page
3. The app connects and starts showing live data

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

See [DESIGN.md](./DESIGN.md) for full build instructions, testing, and architecture documentation.

## Credits

Register map and protocol details sourced from the [givenergy-modbus](https://github.com/dewet22/givenergy-modbus) and [GivTCP](https://github.com/GivTCP/givtcp) open-source projects.

## License

MIT — see [LICENSE](./LICENSE).

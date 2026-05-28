# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-05-28

### Added

- Real-time inverter monitoring: solar, battery, grid, home consumption
- Radial energy flow diagram with live power flows
- Battery page with per-module breakdown (cell voltages, temperatures, SOC, cycles)
- Battery mode control: Eco, Timed Demand, Timed Export, Pause
- Charge/discharge schedule management (time slots + SOC targets)
- SOC reserve, charge rate, and discharge rate controls
- Auto-discovery of dongle serial number from response frame header
- Network scanner for discovering inverters on the local LAN
- WebSocket real-time data streaming to connected clients
- Persistent settings (~/.givenergy-local/settings.json)
- 94 Rust unit tests passing

### Fixed

- Modbus polling resilience: inter-request delay (150ms), stale response retry (4 attempts),
  transient error tolerance (3 consecutive failures before reconnect)
- TCP buffer drain after connect to flush stale dongle responses
- 500ms post-connect delay for slow dongle initialisation
- Settings partial update: Connect button no longer clobbers refresh interval
- Settings version tracking: poll loop detects host changes and reconnects immediately

# GivEnergy Local

Desktop app for monitoring and controlling GivEnergy solar inverters over local Modbus TCP.

## Stack

- **Frontend**: React 19 + TypeScript 6 + Vite 8 + Tailwind CSS 4 + Zustand + Recharts + React Router 7
- **Backend**: Tauri 2 desktop shell; embedded Axum HTTP/WS server on port **7337**
- **Modbus**: Custom Rust TCP client to GivEnergy data adapter (port **8899**)
- **Testing**: Rust unit tests only (no frontend tests, no integration tests)

## Commands

| Command | Action |
|---|---|
| `npm run dev` | Vite dev server on port 5173 |
| `npm run build` | `tsc -b && vite build` (full typecheck + bundle) |
| `npm run lint` | `eslint .` |
| `npm run preview` | `vite preview` |
| `cargo test` (in `src-tauri/`) | Run all Rust unit tests |
| `cargo tauri dev` | Dev mode with Tauri window + Vite + hot-reload |
| `cargo tauri build` | Production build of the desktop app |

Order for full verification: `npm run lint` → `npm run build` (typechecks) → `cargo test` in `src-tauri/`.

## Architecture

- `src/` — React app. Entrypoint: `src/main.tsx`. Pages: `StatusPage`, `HistoryPage`, `ControlPage`, `SettingsPage`.
- `src-tauri/src/` — Rust backend. Key modules:
  - `lib.rs` — Tauri app setup; spawns Axum server (port 7337) + Modbus polling loop
  - `inverter/` — data model, register decode/encode, discovery, poll loop
  - `modbus/` — TCP client, frame protocol, register map
  - `server/` — Axum REST API (`/api/*`) + WebSocket (`/ws`)
  - `settings/` — persisted config (host, serial, port, poll interval)
- Frontend talks to the local Axum server (never directly to the inverter). API base resolves to `http://localhost:7337` in Tauri, or `http://<hostname>:7337` in browser.
- WebSocket at `/ws` streams real-time inverter snapshots and connection state changes.

## TypeScript quirks

- `verbatimModuleSyntax: true` — use `import type` for type-only imports
- `erasableSyntaxOnly: true` — no `enum`, no `namespace`, no `constructor parameter properties`
- `noUnusedLocals` / `noUnusedParameters` — both on; declarations must be used

## Rust testing

All tests are `#[cfg(test)]` unit tests inside each module. Run with:
```
cd src-tauri && cargo test
```
No integration tests or test fixtures exist. The Modbus client tests use a mock TCP server.

## Build artifacts

- `dist/` — Vite output (frontend)
- `src-tauri/target/` — Rust build output
- `node_modules/.tmp/tsconfig.*.tsbuildinfo` — TypeScript incremental build info

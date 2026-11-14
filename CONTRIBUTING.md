# Contributing to GivEnergy Local

Thanks for your interest! This project is a personal tool but suggestions and fixes are welcome.

## Development Setup

On Debian, Ubuntu, or LMDE, install the Linux desktop development packages
before creating a production bundle. These match the project's CI environment:

```bash
sudo apt install \
  libwebkit2gtk-4.1-dev \
  librsvg2-dev \
  libssl-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev
```

Install `rpm` too when creating an RPM bundle. Tauri's tray and AppImage
bundlers need the development packages' pkg-config metadata during
`cargo tauri build`; equivalent runtime-only packages are not sufficient.

Then install the project dependencies and start the desktop app:

```bash
npm install
cd src-tauri && cargo tauri dev
```

## Verification

```bash
# Frontend
npm run lint
npm run build

# Backend
cd src-tauri && cargo test
```

All three must pass before committing.

## Checking Octopus Car Charging by Hand

To see planned Intelligent Octopus Go charges on the Status page without a
real Octopus account or car (needs `npm run build`, `cargo build --release`
and the [GivEnergy Simulator](https://github.com/psylsph/givenergy-simulator)
built at `~/repos/givenergy-simulator`):

```bash
npm run mock:octopus -- --with-hem --slot 23:30-05:30@12 --running 40
```

This starts a mock Octopus API serving a 23:30–05:30 smart charge (12 kWh)
and one running for another 40 minutes, plus the simulator and a throwaway
headless HEM at <http://127.0.0.1:7340> already pointed at the mock, with
Show Car Charging Slots on and the battery hold off. Choose the House view in
Settings → Panel Controls there to see the car line in the Next Charge card.
Your own settings in `~/.givenergy-local` are never touched; Ctrl-C stops
everything and removes the temporary config.

Slots are local times, `HH:MM-HH:MM`, with optional `@kWh` and `:boost`.
`--file slots.json` reads a JSON array of slots on every request, so you can
change them while it runs; `--help` lists every option.

## Code Style

- **Rust**: `cargo fmt` + `cargo clippy`. No integration tests — unit tests only.
- **TypeScript**: `verbatimModuleSyntax` is on — use `import type` for type-only imports. No `enum`, no `namespace`.
- Frontend tests: none currently. Don't add them.

## Pull Requests

- Keep PRs small and focused.
- Include a clear description of the problem and solution.
- Ensure `npm run build` and `cargo test` pass.

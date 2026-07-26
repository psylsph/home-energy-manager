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

## Code Style

- **Rust**: `cargo fmt` + `cargo clippy`. No integration tests — unit tests only.
- **TypeScript**: `verbatimModuleSyntax` is on — use `import type` for type-only imports. No `enum`, no `namespace`.
- Frontend tests: none currently. Don't add them.

## Pull Requests

- Keep PRs small and focused.
- Include a clear description of the problem and solution.
- Ensure `npm run build` and `cargo test` pass.

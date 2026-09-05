#!/bin/bash
# Install a dev-mode .desktop file matching the GTK application ID.
# With enableGTKAppId enabled, the app_id is com.givenergy.local.
# GNOME Wayland matches the app_id to the desktop file ID (filename).
#
# Wired into `npm run dev:desktop` and (transitively) the Tauri
# beforeDevCommand hook in tauri.conf.json, so `cargo tauri dev`
# refreshes this file on every launch and the dock icon never goes
# stale (e.g. after the repo is moved to a new path).
#
# Pass --quiet (or set GIVENERGY_LOCAL_QUIET_DESKTOP=1) to suppress the
# breakdown lines; `beforeDevCommand` does this so the dev terminal stays clean.
set -e

QUIET="${GIVENERGY_LOCAL_QUIET_DESKTOP:-0}"
case "${1:-}" in
  --quiet) QUIET=1 ;;
  "") ;;
  *) echo "Unknown argument: $1" >&2; exit 2 ;;
esac

if [ "$QUIET" = "1" ]; then
  say()  { :; }
  warn() { echo "⚠ $*"; }
else
  say()  { echo "$*"; }
  warn() { echo "⚠ $*"; }
fi

# Desktop Entry string values use backslash escapes, while Exec is a command
# line whose executable argument needs desktop-entry quoting. Keep these
# encodings separate: quotes around Icon would become part of the icon path.
desktop_string_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value// /\\s}"
  value="${value//$'\t'/\\t}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  printf '%s' "$value"
}

desktop_exec_quote() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//\`/\\\`}"
  value="${value//\$/\\\$}"
  printf '"%s"' "$value"
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$APP_DIR/src-tauri/target/debug/givenergy-local"
ICON="$APP_DIR/src-tauri/icons/128x128.png"
EXEC_ENTRY="$(desktop_exec_quote "$BINARY")"
ICON_ENTRY="$(desktop_string_escape "$ICON")"

if [ ! -f "$BINARY" ]; then
  warn "Dev binary not found — build first with: cargo tauri dev"
fi

DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$DESKTOP_DIR"

# Wayland: app_id = "com.givenergy.local" → filename must be com.givenergy.local.desktop
cat > "$DESKTOP_DIR/com.givenergy.local.desktop" << DESKTOP
[Desktop Entry]
Type=Application
Name=Home Energy Manager (Dev)
Exec=$EXEC_ENTRY
Icon=$ICON_ENTRY
Terminal=false
DESKTOP

if command -v update-desktop-database &>/dev/null; then
  update-desktop-database -q "$DESKTOP_DIR" 2>/dev/null || true
fi

say "✓ Installed $DESKTOP_DIR/com.givenergy.local.desktop"
say "  app_id → com.givenergy.local (from tauri.conf.json identifier)"
say "  Icon: $ICON"
say "  Binary: $BINARY"

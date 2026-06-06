#!/bin/bash
# Install a local .desktop file for cargo tauri dev mode.
# GNOME Wayland ignores programmatic window icons — it matches by
# app_id ↔ desktop file ID instead. This script creates a hidden
# desktop entry pointing at the dev binary so the toolbar icon shows.
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$APP_DIR/src-tauri/target/debug/givenergy-local"
ICON="$APP_DIR/src-tauri/icons/128x128.png"

if [ ! -f "$BINARY" ]; then
  echo "⚠ Dev binary not found at $BINARY"
  echo "  Build first with: cargo tauri dev"
  echo "  (or run this script after building — it will work from the second launch)"
fi

DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$DESKTOP_DIR"

cat > "$DESKTOP_DIR/givenergy-local-dev.desktop" << DESKTOP
[Desktop Entry]
Type=Application
Name=Home Energy Manager (Dev)
Exec=$BINARY
Icon=$ICON
Terminal=false
NoDisplay=true
DESKTOP

if command -v update-desktop-database &>/dev/null; then
  update-desktop-database -q "$DESKTOP_DIR" 2>/dev/null || true
fi

echo "✓ Installed $DESKTOP_DIR/givenergy-local-dev.desktop"
echo "  Icon: $ICON"
echo "  Binary: $BINARY"
echo ""
echo "Run the app now with: cd \"$APP_DIR\" && cargo tauri dev"

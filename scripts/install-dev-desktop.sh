#!/bin/bash
# Install a dev-mode .desktop file matching the GTK application ID.
# With enableGTKAppId enabled, the app_id is com.givenergy.local.
# GNOME Wayland matches the app_id to the desktop file ID (filename).
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$APP_DIR/src-tauri/target/debug/givenergy-local"
ICON="$APP_DIR/src-tauri/icons/128x128.png"

if [ ! -f "$BINARY" ]; then
  echo "⚠ Dev binary not found — build first with: cargo tauri dev"
fi

DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$DESKTOP_DIR"

# Wayland: app_id = "com.givenergy.local" → filename must be com.givenergy.local.desktop
cat > "$DESKTOP_DIR/com.givenergy.local.desktop" << DESKTOP
[Desktop Entry]
Type=Application
Name=Home Energy Manager (Dev)
Exec=$BINARY
Icon=$ICON
Terminal=false
DESKTOP

if command -v update-desktop-database &>/dev/null; then
  update-desktop-database -q "$DESKTOP_DIR" 2>/dev/null || true
fi

echo "✓ Installed $DESKTOP_DIR/com.givenergy.local.desktop"
echo "  app_id → com.givenergy.local (from tauri.conf.json identifier)"
echo "  Icon: $ICON"
echo "  Binary: $BINARY"

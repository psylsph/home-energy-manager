#!/bin/bash
# Launch Home Energy Manager.app — handles Gatekeeper + zombie cleanup
# macOS 26.5 blocks ad-hoc signed binaries from /Applications, so this
# launcher copies the app to ~/Desktop if needed, removes quarantine,
# kills stale zombie processes, then launches.

set -euo pipefail

DESKTOP_APP="$HOME/Desktop/Home Energy Manager.app"
APPS_APP="/Applications/Home Energy Manager.app"

# Find the app — prefer Desktop (avoids /Applications block on macOS 26.5)
if [ -x "$DESKTOP_APP/Contents/MacOS/givenergy-local" ]; then
  FOUND="$DESKTOP_APP"
elif [ -x "$APPS_APP/Contents/MacOS/givenergy-local" ]; then
  echo "Copying app from /Applications to Desktop (macOS 26.5 blocks /Applications)..."
  cp -R "$APPS_APP" "$DESKTOP_APP"
  FOUND="$DESKTOP_APP"
else
  echo "Home Energy Manager.app not found on Desktop or in /Applications" >&2
  exit 1
fi

BINARY="$FOUND/Contents/MacOS/givenergy-local"

# 1. Kill any stale zombie givenergy-local processes.
#    A Gatekeeper-zombie has tiny RSS (~8KB), no port 7337 bound.
for pid in $(pgrep -f "givenergy-local" 2>/dev/null || true); do
  [ "$pid" = "$$" ] && continue
  kill "$pid" 2>/dev/null || true
done
sleep 0.5
for pid in $(pgrep -f "givenergy-local" 2>/dev/null || true); do
  [ "$pid" = "$$" ] && continue
  kill -9 "$pid" 2>/dev/null || true
done

# 2. Remove quarantine so it doesn't block the first launch attempt
xattr -d com.apple.quarantine "$FOUND" 2>/dev/null || true
xattr -d com.apple.quarantine "$BINARY" 2>/dev/null || true

# 3. Launch the app from Desktop (works on macOS 26.5)
exec "$BINARY" "$@"

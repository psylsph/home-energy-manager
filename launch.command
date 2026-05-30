#!/bin/bash
# Launch GivEnergy-Local.app bypassing Gatekeeper
# macOS 26.5 blocks ad-hoc signed binaries in /Applications,
# so prefer Desktop over /Applications

for app in \
  "$HOME/Desktop/GivEnergy-Local.app" \
  "/Applications/GivEnergy-Local.app"; do
  if [ -x "$app/Contents/MacOS/givenergy-local" ]; then
    exec "$app/Contents/MacOS/givenergy-local" "$@"
  fi
done
echo "GivEnergy-Local.app not found on Desktop or in /Applications" >&2
exit 1

#!/bin/bash

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT

TEST_HOME="$TEST_ROOT/home"
TEST_BIN="$TEST_ROOT/bin"
APP="$TEST_HOME/Desktop/Home Energy Manager.app"
CAPTURE="$TEST_ROOT/arguments"
XATTR_CAPTURE="$TEST_ROOT/xattr"
mkdir -p "$APP/Contents/MacOS" "$TEST_BIN"

printf '%s\n' \
  '#!/bin/bash' \
  'printf '\''%s\n'\'' "$@" > "$HEM_LAUNCH_CAPTURE"' \
  > "$APP/Contents/MacOS/givenergy-local"
chmod +x "$APP/Contents/MacOS/givenergy-local"

printf '%s\n' '#!/bin/bash' 'exit 1' > "$TEST_BIN/pgrep"
printf '%s\n' \
  '#!/bin/bash' \
  'printf '\''%s\n'\'' "$*" >> "$HEM_XATTR_CAPTURE"' \
  > "$TEST_BIN/xattr"
chmod +x "$TEST_BIN/pgrep" "$TEST_BIN/xattr"

HOME="$TEST_HOME" \
PATH="$TEST_BIN:/usr/bin:/bin" \
HEM_LAUNCH_CAPTURE="$CAPTURE" \
HEM_XATTR_CAPTURE="$XATTR_CAPTURE" \
bash "$ROOT/launch.command" first "two words"

[ "$(wc -l < "$CAPTURE" | tr -d ' ')" -eq 2 ]
[ "$(sed -n '1p' "$CAPTURE")" = "first" ]
[ "$(sed -n '2p' "$CAPTURE")" = "two words" ]
grep -Fq "com.apple.quarantine $APP" "$XATTR_CAPTURE"
grep -Fq "com.apple.quarantine $APP/Contents/MacOS/givenergy-local" "$XATTR_CAPTURE"

echo "macOS launcher checks passed"

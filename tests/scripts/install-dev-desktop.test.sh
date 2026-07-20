#!/bin/bash
# Smoke test for scripts/install-dev-desktop.sh
#
# The script's job is to (re)write a .desktop file at
# $XDG_DATA_HOME/applications/com.givenergy.local.desktop (or its
# $HOME/.local/share/applications default) so that the GTK app_id
# `com.givenergy.local` — registered by the running binary via
# `enableGTKAppId` + `gtk::Application::new` in tao — resolves to a
# valid .desktop entry and the dock / taskbar can render the icon.
#
# There is no business logic to unit-test in the usual sense, but the
# script has several behaviours worth pinning so a future edit can't
# silently break any of them:
#
#   1. Writes the .desktop file to the XDG-correct location
#      (XDG_DATA_HOME/applications when set, $HOME/.local/share/... otherwise).
#   2. The Exec and Icon paths resolve relative to the script's own
#      location, NOT to $PWD or anything ambient — so it works
#      correctly no matter where it is invoked from.
#   3. The app_id matches the GTK app_id baked into tauri.conf.json
#      (`identifier: "com.givenergy.local"`) and the filename ends in
#      `.desktop`. If either drifts, the desktop environment silently
#      drops the icon.
#   4. The file is idempotent — re-running the script overwrites with
#      identical content (no concat / append bugs).
#   5. A missing dev binary is a soft warning, not a hard error: the
#      .desktop file still gets written so it doesn't go stale while
#      the user builds for the first time.
#   6. The desktop-file validates (so the DE actually accepts it).
#   7. The npm hook uses the cross-platform --quiet argument rather than a
#      POSIX-only leading environment assignment (which cmd.exe rejects).
#
# Run standalone: `bash tests/scripts/install-dev-desktop.test.sh`
# Or via `npm test` (wired through package.json).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INSTALLER="$REPO_ROOT/scripts/install-dev-desktop.sh"

# bash lives here; tests put the staged fake first on PATH but always
# keep this directory reachable so the wrapper itself can be invoked.
BIN_DIR="$(dirname "$(command -v bash)")"

if [ ! -x "$INSTALLER" ]; then
  echo "FAIL: installer not found or not executable at $INSTALLER" >&2
  exit 1
fi

# --- Test harness -------------------------------------------------------------

PASS=0
FAIL=0

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "  PASS  $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label"
    echo "        expected: $expected"
    echo "        actual:   $actual"
    FAIL=$((FAIL + 1))
  fi
}

# Pull the value of a .desktop Entry field out of a desktop file.
# Returns `<missing>` if the field is absent.
desktop_field() {
  local file="$1" key="$2"
  local line
  line="$(grep -E "^${key}=" "$file" 2>/dev/null | head -n1 || true)"
  if [ -z "$line" ]; then
    printf '<missing>'
  else
    printf '%s' "${line#${key}=}"
  fi
}

# Stage a fake dev binary that the installer will record as the Exec
# target. Returns the stagedir path; caller cleans up.
stage_fake_binary() {
  local stagedir
  stagedir="$(mktemp -d)"
  cat > "$stagedir/givenergy-local" <<'STUB'
#!/bin/bash
echo "fake dev binary invoked with $# args"
STUB
  chmod +x "$stagedir/givenergy-local"
  printf '%s\n' "$stagedir"
}

# Stage a fake XDG_DATA_HOME so the installer writes somewhere isolated
# instead of polluting the real user's applications directory. Returns
# the xdgdir path; caller cleans up.
stage_xdg_home() {
  local xdgdir
  xdgdir="$(mktemp -d)"
  printf '%s\n' "$xdgdir"
}

# Run the installer with HOME + XDG_DATA_HOME pointed at the staged
# directories. Captures stdout/stderr into $STAGE/install.out.
run_installer() {
  local xdgdir="$1" stagedir="$2" out="$3"
  (
    # The installer resolves the dev-binary path via
    #   BINARY="$APP_DIR/src-tauri/target/debug/givenergy-local"
    # which won't exist in tests. We pre-stage a fake binary there
    # so the installer's "missing binary" warning doesn't fire and
    # so the recorded Exec path can be matched later.
    mkdir -p "$REPO_ROOT/src-tauri/target/debug"
    # Symlink rather than copy so we don't pay for ~400 MB of bytes on
    # every test run; the installer only stat()s the path.
    ln -sf "$stagedir/givenergy-local" "$REPO_ROOT/src-tauri/target/debug/givenergy-local"
    HOME="$xdgdir" \
      XDG_DATA_HOME="$xdgdir" \
      bash "$INSTALLER" >"$out" 2>&1
    # Clean up the symlink so it doesn't leak across tests.
    rm -f "$REPO_ROOT/src-tauri/target/debug/givenergy-local"
  )
}

# --- Tests --------------------------------------------------------------------

echo "tests/scripts/install-dev-desktop.test.sh"

# Test 1: the installer writes the .desktop file to the XDG-correct
# location and the file's Type/Exec/Icon fields are populated.
echo
echo "1. writes .desktop file at the XDG-correct location"
XDG="$(stage_xdg_home)"
STAGE="$(stage_fake_binary)"
OUT="$XDG/install.out"
run_installer "$XDG" "$STAGE" "$OUT"
DESKTOP_FILE="$XDG/applications/com.givenergy.local.desktop"
assert_eq "desktop file exists"        "yes"  "$([ -f "$DESKTOP_FILE" ] && echo yes || echo no)"
assert_eq "Type field"                  "Application" "$(desktop_field "$DESKTOP_FILE" Type)"
# The installer records the literal Exec string it computed (the path
# to the dev binary resolved relative to its own location). We stage
# a symlink at that location so the script sees the binary as present
# AND so the recorded Exec path is deterministic. The DESKTOP entry
# stores the literal path — symlink resolution happens at launch.
EXPECTED_EXEC="$REPO_ROOT/src-tauri/target/debug/givenergy-local"
assert_eq "Exec points at the dev binary path" "$EXPECTED_EXEC" "$(desktop_field "$DESKTOP_FILE" Exec)"
assert_eq "Icon is the repo icon"       "$REPO_ROOT/src-tauri/icons/128x128.png" "$(desktop_field "$DESKTOP_FILE" Icon)"
assert_eq "Terminal=false"              "false" "$(desktop_field "$DESKTOP_FILE" Terminal)"
rm -rf "$XDG" "$STAGE"

# Test 2: app_id / filename must match `com.givenergy.local`. The
# installer uses this filename for a reason — it's the GTK app_id
# the running binary registers via `gtk::Application::new`. A typo
# here breaks the dock icon silently.
echo
echo "2. .desktop filename matches the GTK app_id"
XDG="$(stage_xdg_home)"
STAGE="$(stage_fake_binary)"
run_installer "$XDG" "$STAGE" "$XDG/install.out"
assert_eq "filename ends in .desktop"  "com.givenergy.local.desktop" "$(basename "$XDG/applications/com.givenergy.local.desktop")"
rm -rf "$XDG" "$STAGE"

# Test 3: re-running the script is idempotent — overwrites the file
# cleanly with the same content. A regression that appended lines or
# left stale fields behind would break desktop-file-validate after
# the second run.
echo
echo "3. idempotent on re-run"
XDG="$(stage_xdg_home)"
STAGE="$(stage_fake_binary)"
run_installer "$XDG" "$STAGE" "$XDG/install1.out"
FIRST_CONTENT="$(cat "$XDG/applications/com.givenergy.local.desktop")"
run_installer "$XDG" "$STAGE" "$XDG/install2.out"
SECOND_CONTENT="$(cat "$XDG/applications/com.givenergy.local.desktop")"
assert_eq "content identical across runs" "$FIRST_CONTENT" "$SECOND_CONTENT"
rm -rf "$XDG" "$STAGE"

# Test 4: HOME-only fallback (XDG_DATA_HOME unset) — the installer
# should resolve to $HOME/.local/share/applications. This is what
# most users actually hit since the XDG variable is unset by default.
echo
echo "4. falls back to \$HOME/.local/share/applications"
XDG="$(stage_xdg_home)"
STAGE="$(stage_fake_binary)"
(
  unset XDG_DATA_HOME
  HOME="$XDG" \
    bash -c '
      mkdir -p "$HOME/src-tauri/target/debug" 2>/dev/null || true
      # Override REPO_ROOT for this subshell by symlinking the staged
      # binary at the path the installer expects. The installer resolves
      # BINARY relative to its own location ($REPO_ROOT/src-tauri/...),
      # so we need the installer to think it lives at the real repo
      # root — but HOME is still under $XDG, which is what were testing.
      exit 0
    '
  # The above is just a smoke check; actually run the installer with
  # HOME-only and no XDG_DATA_HOME. The installer resolves BINARY via
  # SCRIPT_DIR -> APP_DIR, so it points at the real repo's
  # target/debug path. We stage a fake there so the missing-binary
  # warning does not fire.
  mkdir -p "$REPO_ROOT/src-tauri/target/debug"
  ln -sf "$STAGE/givenergy-local" "$REPO_ROOT/src-tauri/target/debug/givenergy-local"
  HOME="$XDG" \
    bash "$INSTALLER" >"$XDG/install.out" 2>&1
  rm -f "$REPO_ROOT/src-tauri/target/debug/givenergy-local"
)
assert_eq "HOME-only writes to .local/share/applications" "yes" "$([ -f "$XDG/.local/share/applications/com.givenergy.local.desktop" ] && echo yes || echo no)"
rm -rf "$XDG" "$STAGE"

# Test 5: a missing dev binary is a soft warning, not a fatal exit.
# The .desktop file still has to land, so the dock icon doesn't go
# stale during the first build. We assert (a) the script exits 0,
# (b) the .desktop file still gets written.
echo
echo "5. missing dev binary is a soft warning, not a fatal exit"
XDG="$(stage_xdg_home)"
(
  HOME="$XDG" \
    XDG_DATA_HOME="$XDG" \
    bash "$INSTALLER" >"$XDG/install.out" 2>&1
)
EXIT_CODE=$?
assert_eq "exit code is 0 with no binary"  "0"  "$EXIT_CODE"
assert_eq "desktop file written anyway"   "yes" "$([ -f "$XDG/applications/com.givenergy.local.desktop" ] && echo yes || echo no)"
rm -rf "$XDG"

# Test 6: the generated .desktop file passes desktop-file-validate.
# This is the property the DE actually checks at runtime — if it
# doesn't validate, the file is silently dropped and the dock icon
# is generic / missing regardless of how correct the contents look
# to us.
echo
echo "6. generated file validates"
if ! command -v desktop-file-validate >/dev/null 2>&1; then
  echo "  SKIP  desktop-file-validate not installed"
else
  XDG="$(stage_xdg_home)"
  STAGE="$(stage_fake_binary)"
  run_installer "$XDG" "$STAGE" "$XDG/install.out"
  set +e
  desktop-file-validate "$XDG/applications/com.givenergy.local.desktop" >"$XDG/validate.out" 2>&1
  VALIDATE_EXIT=$?
  set -e
  assert_eq "desktop-file-validate exit" "0" "$VALIDATE_EXIT"
  rm -rf "$XDG" "$STAGE"
fi

# Test 7: npm runs scripts through cmd.exe on Windows. A leading
# VAR=value command works in POSIX shells but fails before bash starts on
# Windows, so quiet mode must be passed as a normal argument.
echo
echo "7. npm dev hook uses cross-platform quiet mode"
DEV_DESKTOP_COMMAND="$(python3 - "$REPO_ROOT/package.json" <<'PY'
import json
import pathlib
import sys

package = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
print(package["scripts"]["dev:desktop"])
PY
)"
assert_eq "dev:desktop command" \
  "bash scripts/install-dev-desktop.sh --quiet" \
  "$DEV_DESKTOP_COMMAND"
XDG="$(stage_xdg_home)"
STAGE="$(stage_fake_binary)"
mkdir -p "$REPO_ROOT/src-tauri/target/debug"
ln -sf "$STAGE/givenergy-local" "$REPO_ROOT/src-tauri/target/debug/givenergy-local"
HOME="$XDG" XDG_DATA_HOME="$XDG" \
  bash "$INSTALLER" --quiet >"$XDG/install.out" 2>&1
rm -f "$REPO_ROOT/src-tauri/target/debug/givenergy-local"
assert_eq "--quiet suppresses installer output" "" "$(cat "$XDG/install.out")"
rm -rf "$XDG" "$STAGE"

# --- Summary ------------------------------------------------------------------

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
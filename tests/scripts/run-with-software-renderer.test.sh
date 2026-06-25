#!/bin/bash
# Smoke test for scripts/run-with-software-renderer.sh
#
# The wrapper has two responsibilities: (a) set the WebKitGTK env vars
# before exec'ing the binary, and (b) forward all argv. There is no
# business logic to unit-test — the value of this file is in pinning
# the contract so a future edit can't silently drop an env var or
# swallow an argument.
#
# Run standalone: `bash tests/scripts/run-with-software-renderer.test.sh`
# Or via `npm test` (wired through package.json).
#
# Strategy: stage a fake `givenergy-local` binary on PATH that records
# its environment + argv to a temp file, invoke the wrapper with
# various inputs, then assert against the recorded file.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WRAPPER="$REPO_ROOT/scripts/run-with-software-renderer.sh"

# bash lives here; tests put the staged fake first on PATH but always
# keep this directory reachable so the wrapper itself can be invoked.
BIN_DIR="$(dirname "$(command -v bash)")"

if [ ! -x "$WRAPPER" ]; then
  echo "FAIL: wrapper not found or not executable at $WRAPPER" >&2
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

# Stage a temp dir with a fake givenergy-local that records its env + argv.
# Returns the directory path; caller is responsible for cleanup.
stage_fake_binary() {
  local stagedir
  stagedir="$(mktemp -d)"
  cat > "$stagedir/givenergy-local" <<'STUB'
#!/bin/bash
{
  echo "argv-count=$#"
  for a in "$@"; do echo "argv=$a"; done
  echo "env:WEBKIT_DISABLE_DMABUF_RENDERER=${WEBKIT_DISABLE_DMABUF_RENDERER:-<unset>}"
  echo "env:WEBKIT_DISABLE_COMPOSITING_MODE=${WEBKIT_DISABLE_COMPOSITING_MODE:-<unset>}"
  echo "env:GDK_BACKEND=${GDK_BACKEND:-<unset>}"
  echo "env:XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-<unset>}"
} > "$GIVENERGY_LOCAL_RECORD"
STUB
  chmod +x "$stagedir/givenergy-local"
  printf '%s\n' "$stagedir"
}

# Pull a field out of the fake-binary's record file. Returns
# `<missing>` if the field was never recorded (the fake didn't see
# that env var at all, which would itself be a test failure).
field() {
  local record="$1" key="$2"
  local line
  line="$(grep -F "$key=" "$record" 2>/dev/null | head -n1 || true)"
  if [ -z "$line" ]; then
    printf '<missing>'
  else
    printf '%s' "${line#${key}=}"
  fi
}

# --- Tests --------------------------------------------------------------------

echo "tests/scripts/run-with-software-renderer.test.sh"

# Test 1: clean invocation on an X11 session sets both vars to 1,
# auto-sets GDK_BACKEND=x11, and forwards no extra argv.
echo
echo "1. clean invocation (X11 session)"
STAGE="$(stage_fake_binary)"
(
  unset GDK_BACKEND XDG_SESSION_TYPE WEBKIT_DISABLE_DMABUF_RENDERER WEBKIT_DISABLE_COMPOSITING_MODE
  # Keep bash on PATH (it's in BIN_DIR, usually /usr/bin) but put the
  # staged fake binary first so the wrapper's `command -v givenergy-local`
  # lookup resolves to our recording stub instead of any system install.
  PATH="$STAGE:$BIN_DIR" \
    XDG_SESSION_TYPE=x11 \
    GIVENERGY_LOCAL_RECORD="$STAGE/record.out" \
    bash "$WRAPPER" >/dev/null 2>&1
)
assert_eq "WEBKIT_DISABLE_DMABUF_RENDERER defaults to 1" "1"   "$(field "$STAGE/record.out" env:WEBKIT_DISABLE_DMABUF_RENDERER)"
assert_eq "WEBKIT_DISABLE_COMPOSITING_MODE defaults to 1" "1"   "$(field "$STAGE/record.out" env:WEBKIT_DISABLE_COMPOSITING_MODE)"
assert_eq "GDK_BACKEND auto-set on X11"                "x11" "$(field "$STAGE/record.out" env:GDK_BACKEND)"
assert_eq "argv-count is zero"                          "0"   "$(field "$STAGE/record.out" argv-count)"
rm -rf "$STAGE"

# Test 2: Wayland session leaves GDK_BACKEND unset (we don't second-guess).
echo
echo "2. clean invocation (Wayland session)"
STAGE="$(stage_fake_binary)"
(
  unset GDK_BACKEND XDG_SESSION_TYPE WEBKIT_DISABLE_DMABUF_RENDERER WEBKIT_DISABLE_COMPOSITING_MODE
  PATH="$STAGE:$BIN_DIR" \
    XDG_SESSION_TYPE=wayland \
    GIVENERGY_LOCAL_RECORD="$STAGE/record.out" \
    bash "$WRAPPER" >/dev/null 2>&1
)
assert_eq "GDK_BACKEND unset on Wayland" "<unset>" "$(field "$STAGE/record.out" env:GDK_BACKEND)"
rm -rf "$STAGE"

# Test 3: argv passthrough — the wrapper's `exec "$BINARY" "$@"`
# forwards every arg verbatim, so the fake binary should see the same
# argc and argv that the wrapper was invoked with.
echo
echo "3. argv passthrough"
STAGE="$(stage_fake_binary)"
(
  unset GDK_BACKEND XDG_SESSION_TYPE WEBKIT_DISABLE_DMABUF_RENDERER WEBKIT_DISABLE_COMPOSITING_MODE
  PATH="$STAGE:$BIN_DIR" \
    XDG_SESSION_TYPE=x11 \
    GIVENERGY_LOCAL_RECORD="$STAGE/record.out" \
    bash "$WRAPPER" --headless --help >/dev/null 2>&1
)
assert_eq "argv-count is two"                "2"          "$(field "$STAGE/record.out" argv-count)"
assert_eq "first argv"                       "--headless" "$(grep -F 'argv=--headless' "$STAGE/record.out" | head -n1 | sed 's/^argv=//')"
assert_eq "second argv"                      "--help"     "$(grep -F 'argv=--help' "$STAGE/record.out" | head -n1 | sed 's/^argv=//')"
rm -rf "$STAGE"

# Test 4: user override via the env flows through unchanged. The wrapper
# uses ${VAR:-1}, which substitutes the default whenever the var is
# unset OR empty. So to test pass-through we need an explicit non-empty
# value (e.g. `WEBKIT_DISABLE_COMPOSITING_MODE=0`); the default only
# applies when the user has neither set the var nor expressed intent
# via an empty value.
echo
echo "4. user overrides pass through"
STAGE="$(stage_fake_binary)"
(
  unset GDK_BACKEND XDG_SESSION_TYPE
  PATH="$STAGE:$BIN_DIR" \
    WEBKIT_DISABLE_DMABUF_RENDERER=0 \
    WEBKIT_DISABLE_COMPOSITING_MODE=0 \
    GIVENERGY_LOCAL_RECORD="$STAGE/record.out" \
    bash "$WRAPPER" >/dev/null 2>&1
)
assert_eq "explicit 0 override is preserved (DMABUF)"   "0"     "$(field "$STAGE/record.out" env:WEBKIT_DISABLE_DMABUF_RENDERER)"
assert_eq "explicit 0 override is preserved (COMPOSITE)" "0"    "$(field "$STAGE/record.out" env:WEBKIT_DISABLE_COMPOSITING_MODE)"
rm -rf "$STAGE"

# Test 5: a user-set GDK_BACKEND is preserved (we only auto-set when unset).
echo
echo "5. explicit GDK_BACKEND is preserved"
STAGE="$(stage_fake_binary)"
(
  unset XDG_SESSION_TYPE WEBKIT_DISABLE_DMABUF_RENDERER WEBKIT_DISABLE_COMPOSITING_MODE
  PATH="$STAGE:$BIN_DIR" \
    GDK_BACKEND=wayland \
    XDG_SESSION_TYPE=x11 \
    GIVENERGY_LOCAL_RECORD="$STAGE/record.out" \
    bash "$WRAPPER" >/dev/null 2>&1
)
assert_eq "user-set GDK_BACKEND wins"   "wayland" "$(field "$STAGE/record.out" env:GDK_BACKEND)"
rm -rf "$STAGE"

# Test 6: missing binary fails cleanly with exit 127. We need bash on
# PATH (to invoke the wrapper) but no givenergy-local anywhere — neither
# /usr/bin/givenergy-local (won't exist in CI) nor anywhere on PATH.
echo
echo "6. missing binary exits 127"
EMPTY_STAGE="$(mktemp -d)"
EXIT_CODE=$(
  set +e
  unset GDK_BACKEND XDG_SESSION_TYPE WEBKIT_DISABLE_DMABUF_RENDERER WEBKIT_DISABLE_COMPOSITING_MODE
  # EMPTY_STAGE is empty so `command -v givenergy-local` cannot find
  # anything; /usr/bin/givenergy-local also won't exist in CI.
  PATH="$EMPTY_STAGE:$BIN_DIR" \
    bash "$WRAPPER" >/dev/null 2>&1
  printf '%d' $?
)
rm -rf "$EMPTY_STAGE"
assert_eq "exit code when binary missing" "127" "$EXIT_CODE"

# --- Summary ------------------------------------------------------------------

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0

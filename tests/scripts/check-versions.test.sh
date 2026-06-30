#!/bin/bash
# Tests for scripts/check-versions.sh
#
# The guard's contract: given a checkout root containing package.json,
# src-tauri/Cargo.toml, and src-tauri/tauri.conf.json, exit 0 iff all
# three declare the same version; otherwise exit 1 and name the culprits.
# These tests pin that contract against a staged temp tree so a future
# edit can't silently break the parser (the JSON-indent anchor, the
# Cargo `^version` anchor) or the three-way comparison.
#
# Run standalone: `bash tests/scripts/check-versions.test.sh`
# Or via `npm test` (wired through package.json).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CHECK="$REPO_ROOT/scripts/check-versions.sh"

if [ ! -x "$CHECK" ]; then
  echo "FAIL: checker not found or not executable at $CHECK" >&2
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

# Build a fake checkout at $root with the three files set to the given
# versions. Empty string = omit the version line (parse-failure case).
# The Cargo.toml fixture deliberately includes an inline dependency with
# its own `version` to prove the parser anchors on the package version,
# not a nested dependency version.
write_tree() {
  local root="$1" pkg="$2" cargo="$3" tauri="$4"
  mkdir -p "$root/src-tauri"
  {
    echo '{'
    echo '  "name": "givenergy-local",'
    if [ -n "$pkg" ]; then printf '  "version": "%s",\n' "$pkg"; fi
    echo '  "description": "stub"'
    echo '}'
  } > "$root/package.json"
  {
    echo '[package]'
    echo 'name = "givenergy-local"'
    if [ -n "$cargo" ]; then printf 'version = "%s"\n' "$cargo"; fi
    echo 'edition = "2021"'
    echo
    echo '[dependencies]'
    # Inline dep form — must NOT be mistaken for the package version.
    echo 'serde = { version = "1.0", features = ["derive"] }'
  } > "$root/src-tauri/Cargo.toml"
  {
    echo '{'
    echo '  "$schema": "https://schema.tauri.app/config/2",'
    if [ -n "$tauri" ]; then printf '  "version": "%s",\n' "$tauri"; fi
    echo '  "productName": "stub"'
    echo '}'
  } > "$root/src-tauri/tauri.conf.json"
}

# --- Tests --------------------------------------------------------------------

echo "tests/scripts/check-versions.test.sh"

# Test 1: all three agree → exit 0. The Cargo fixture includes
# `serde = { version = "1.0" }`, so passing here also proves we parsed
# 1.2.3 (the package version) rather than 1.0 (the dependency version).
echo
echo "1. all three in sync exits 0"
STAGE="$(mktemp -d)"
write_tree "$STAGE" 1.2.3 1.2.3 1.2.3
set +e
OUT="$(ROOT="$STAGE" bash "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "exit code"           "0" "$RC"
assert_eq "mentions in sync"    "1" "$([ $(( $(printf '%s' "$OUT" | grep -c 'in sync') )) -gt 0 ] && echo 1 || echo 0)"
rm -rf "$STAGE"

# Test 2: tauri.conf.json lags (the exact shape of the v0.33.2 bug).
echo
echo "2. tauri.conf.json lags the other two"
STAGE="$(mktemp -d)"
write_tree "$STAGE" 0.33.2 0.33.2 0.33.1
set +e
OUT="$(ROOT="$STAGE" bash "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "exit code"            "1" "$RC"
assert_eq "names tauri.conf.json" "1" "$(printf '%s' "$OUT" | grep -c 'tauri.conf.json')"
rm -rf "$STAGE"

# Test 3: Cargo.toml is the outlier.
echo
echo "3. Cargo.toml differs"
STAGE="$(mktemp -d)"
write_tree "$STAGE" 2.0.0 1.9.9 2.0.0
set +e
OUT="$(ROOT="$STAGE" bash "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "exit code"       "1" "$RC"
assert_eq "names Cargo.toml" "1" "$(printf '%s' "$OUT" | grep -c 'Cargo.toml')"
rm -rf "$STAGE"

# Test 4: package.json is the outlier.
echo
echo "4. package.json differs"
STAGE="$(mktemp -d)"
write_tree "$STAGE" 5.0.0 5.1.0 5.1.0
set +e
OUT="$(ROOT="$STAGE" bash "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "exit code"          "1" "$RC"
assert_eq "names package.json" "1" "$(printf '%s' "$OUT" | grep -c 'package.json')"
rm -rf "$STAGE"

# Test 5: a missing version line is a parse failure (exit 1), not an
# abrupt `set -e` abort — the guard must keep going and report which
# file it couldn't parse.
echo
echo "5. missing version line is a parse failure"
STAGE="$(mktemp -d)"
write_tree "$STAGE" "" 1.0.0 1.0.0
set +e
OUT="$(ROOT="$STAGE" bash "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "exit code"                 "1" "$RC"
assert_eq "reports could-not-parse"    "1" "$(printf '%s' "$OUT" | grep -c 'could not parse')"
rm -rf "$STAGE"

# Test 6: smoke test against the real repo — the guard must pass on the
# actual checkout right now, and the no-arg default path (ROOT resolved
# from the script's own location) must work, not just the ROOT override.
echo
echo "6. real repo is in sync (default ROOT path)"
set +e
OUT="$(bash "$CHECK" 2>&1)"
RC=$?
set -e
assert_eq "exit code"   "0" "$RC"
rm -f /tmp/.unused 2>/dev/null || true

# --- Summary ------------------------------------------------------------------

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0

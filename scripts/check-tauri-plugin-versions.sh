#!/bin/bash
# Verify that every JavaScript Tauri plugin package the frontend
# imports (@tauri-apps/plugin-*) is paired with the same major/minor
# release of its Rust crate (tauri-plugin-*).
#
# tauri-cli hard-fails `tauri build` when the two drift:
#
#   Error Found version mismatched Tauri packages. Make sure the NPM package
#   and Rust crate versions are on the same major/minor releases:
#   tauri-plugin-opener (v2.6.0) : @tauri-apps/plugin-opener (v2.5.4)
#
# That is what killed every platform job of the v0.85.0 release (run
# 37033633298): Dependabot bumped the crates alone, so each platform job
# spent its setup minutes only to die before compiling anything.
#
# The versions come from package-lock.json (what `npm ci` installs) and
# src-tauri/Cargo.lock (what cargo compiles) — the same inputs the
# bundler reads, with no node_modules or network needed.
#
# Usage:
#   scripts/check-tauri-plugin-versions.sh                 # this repo
#   scripts/check-tauri-plugin-versions.sh /path/to/repo    # another checkout
#   ROOT=/path scripts/check-tauri-plugin-versions.sh       # … or via ROOT (used by tests)
#
# Exit 0 when every pair agrees, 1 otherwise (naming each drifting pair).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Default to the repo this script ships in; allow a ROOT override or a
# positional arg so tests can point this at a staged temp tree.
ROOT="${ROOT:-${1:-$(cd "$SCRIPT_DIR/.." && pwd)}}"

die() { echo "✗ $*" >&2; exit 1; }

PKG_LOCK="$ROOT/package-lock.json"
CARGO_LOCK="$ROOT/src-tauri/Cargo.lock"
[ -f "$PKG_LOCK" ]   || die "not found: $PKG_LOCK"
[ -f "$CARGO_LOCK" ] || die "not found: $CARGO_LOCK"
command -v node >/dev/null 2>&1 || die "node is required to read $PKG_LOCK"

major_minor() { printf '%s' "$1" | cut -d. -f1,2; }

# crate_version <crate-name>: the version of that crate in Cargo.lock, or
# empty if it is absent. `version` only counts inside the matching
# [[package]] block, so a neighbouring package can't supply it.
crate_version() {
  awk -v want="$1" '
    $0 == "name = \"" want "\"" { hit = 1; next }
    hit && /^version = "/ { gsub(/^version = "/, ""); gsub(/"$/, ""); print; exit }
  ' "$CARGO_LOCK"
}

# One "name version" line per installed @tauri-apps/plugin-* package.
# package-lock.json is JSON, so let node do the parsing rather than
# pattern-matching braces.
JS_PLUGINS="$(node -e '
  const fs = require("fs");
  const path = require("path");
  const lock = JSON.parse(fs.readFileSync(path.resolve(process.argv[1]), "utf8"));
  const found = [];
  for (const [key, entry] of Object.entries(lock.packages || {})) {
    const match = /^node_modules\/@tauri-apps\/plugin-(.+)$/.exec(key);
    if (match && typeof entry.version === "string") found.push(match[1] + " " + entry.version);
  }
  found.sort();
  process.stdout.write(found.join("\n"));
' "$PKG_LOCK")"

MISMATCHED=0
CHECKED=0
REPORT=""

while IFS= read -r line; do
  [ -n "$line" ] || continue
  name="${line%% *}"
  js_version="${line##* }"
  crate="tauri-plugin-$name"
  crate_ver="$(crate_version "$crate")"

  if [ -z "$crate_ver" ]; then
    REPORT="${REPORT}    @tauri-apps/plugin-$name : $js_version  (no $crate in src-tauri/Cargo.lock)
"
    MISMATCHED=1
    continue
  fi

  CHECKED=$((CHECKED + 1))
  if [ "$(major_minor "$js_version")" != "$(major_minor "$crate_ver")" ]; then
    REPORT="${REPORT}    @tauri-apps/plugin-$name : $js_version
    $crate : $crate_ver
"
    MISMATCHED=1
  fi
done <<< "$JS_PLUGINS"

if [ "$MISMATCHED" -ne 0 ]; then
  echo "✗ Tauri plugin version mismatch between the npm package and its Rust crate:" >&2
  printf '%s' "$REPORT" >&2
  echo "    tauri build refuses to run unless each pair shares a major/minor release." >&2
  echo "    Bump both sides together — see AGENTS.md → \"Tauri plugin pairing\"." >&2
  exit 1
fi

if [ "$CHECKED" -eq 0 ]; then
  echo "✓ tauri plugin versions in sync (no @tauri-apps/plugin-* packages installed)"
  exit 0
fi

echo "✓ tauri plugin versions in sync"
while IFS= read -r line; do
  [ -n "$line" ] || continue
  name="${line%% *}"
  js_version="${line##* }"
  printf '    @tauri-apps/plugin-%s %s ↔ tauri-plugin-%s %s\n' \
    "$name" "$js_version" "$name" "$(crate_version "tauri-plugin-$name")"
done <<< "$JS_PLUGINS"

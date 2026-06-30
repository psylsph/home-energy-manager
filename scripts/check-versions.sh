#!/bin/bash
# Verify that package.json, src-tauri/Cargo.toml, and
# src-tauri/tauri.conf.json all declare the same version.
#
# The release process (AGENTS.md → "Release process") bumps all three by
# hand. This guard catches the "bumped two, forgot one" mistake — the same
# class of bug that shipped v0.33.2 with tauri.conf.json still reading
# 0.33.1 while the other two files said 0.33.2 (see the version audit).
#
# Usage:
#   scripts/check-versions.sh                 # check the repo this script ships in
#   scripts/check-versions.sh /path/to/repo   # check a specific checkout
#   ROOT=/path scripts/check-versions.sh      # … or via the ROOT env var (used by tests)
#   npm run check:versions                    # convenience wrapper
#
# Exit 0 if all three agree, 1 otherwise (with a breakdown of the culprits).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Default to the repo the script ships in; allow a ROOT override or a
# positional arg so tests can point this at a staged temp tree.
ROOT="${ROOT:-${1:-$(cd "$SCRIPT_DIR/.." && pwd)}}"

die() { echo "✗ $*" >&2; exit 1; }

[ -f "$ROOT/package.json" ]             || die "not found: $ROOT/package.json"
[ -f "$ROOT/src-tauri/Cargo.toml" ]     || die "not found: $ROOT/src-tauri/Cargo.toml"
[ -f "$ROOT/src-tauri/tauri.conf.json" ] || die "not found: $ROOT/src-tauri/tauri.conf.json"

# Extract the declared version from each file.
#
#   package.json / tauri.conf.json — top-level `"version": "x"`, anchored on
#     exactly two leading spaces so a nested dependency's version field
#     (indented further) can never match.
#
#   Cargo.toml — a bare `version = "x"` at line start. By cargo convention
#     the [package] table comes first, so its version is the first such
#     line; dependency versions appear inline as `serde = { version = "1" }`
#     or `serde = "1"` and never as a bare leading `version =`.
#
# grep is allowed to find nothing (returns 1); we then treat an empty result
# as a parse failure below rather than letting `set -e` abort silently.
read_pkg_version() {
  local line
  line="$(grep -m1 -E '^  "version":' "$1" 2>/dev/null || true)"
  printf '%s' "$line" | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/'
}

read_cargo_version() {
  local line
  line="$(grep -m1 -E '^version[[:space:]]*=' "$1" 2>/dev/null || true)"
  printf '%s' "$line" | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/'
}

PKG=$(read_pkg_version   "$ROOT/package.json")
CARGO=$(read_cargo_version "$ROOT/src-tauri/Cargo.toml")
TAURI=$(read_pkg_version "$ROOT/src-tauri/tauri.conf.json")

[ -n "$PKG" ]   || die "could not parse version from $ROOT/package.json"
[ -n "$CARGO" ] || die "could not parse version from $ROOT/src-tauri/Cargo.toml"
[ -n "$TAURI" ] || die "could not parse version from $ROOT/src-tauri/tauri.conf.json"

if [ "$PKG" = "$CARGO" ] && [ "$CARGO" = "$TAURI" ]; then
  echo "✓ versions in sync: $PKG"
  echo "    package.json, src-tauri/Cargo.toml, src-tauri/tauri.conf.json"
  exit 0
fi

echo "✗ version mismatch across release files:" >&2
printf '    package.json              : %s\n' "$PKG"   >&2
printf '    src-tauri/Cargo.toml      : %s\n' "$CARGO" >&2
printf '    src-tauri/tauri.conf.json : %s\n' "$TAURI" >&2
echo    "    Bump all three together — see AGENTS.md → Release process." >&2
exit 1

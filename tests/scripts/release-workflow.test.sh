#!/bin/bash
# Tests for the release workflow's publish ordering.
#
# Issue #291: each platform build used to publish the GitHub release as
# soon as its own assets were uploaded, so `releases/latest` flipped to
# the new version while the other platforms' installers were still
# missing — updaters that read "latest" in that window got 404s. The
# workflow must now upload everything to a draft release and only flip
# it public from a final job that first verifies every installer is
# present.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKFLOW="$REPO_ROOT/.github/workflows/build.yml"

PASS=0
FAIL=0

assert_contains() {
  local label="$1" needle="$2" haystack="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "  PASS  $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label"
    echo "        missing: $needle"
    FAIL=$((FAIL + 1))
  fi
}

assert_not_contains() {
  local label="$1" needle="$2" haystack="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "  PASS  $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label"
    echo "        unexpected: $needle"
    FAIL=$((FAIL + 1))
  fi
}

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

WORKFLOW_YAML="$(cat "$WORKFLOW")"
# grep -c exits 1 on zero matches; keep set -e from aborting the test.
count_matches() {
  grep -c -- "$1" "$WORKFLOW" || true
}

echo "tests/scripts/release-workflow.test.sh"
echo

echo "1. platform builds upload to a draft release, never publish directly"
UPLOAD_COUNT="$(count_matches 'softprops/action-gh-release')"
DRAFT_TRUE_COUNT="$(count_matches 'draft: true')"
assert_eq "both platform jobs upload release assets" "2" "$UPLOAD_COUNT"
assert_eq "every upload step targets a draft release" "$UPLOAD_COUNT" "$DRAFT_TRUE_COUNT"
assert_not_contains "no upload step publishes the release itself" "draft: false" "$WORKFLOW_YAML"

echo
echo "2. a final job publishes only after every platform asset is verified"
assert_contains "publish job exists" "publish-release:" "$WORKFLOW_YAML"
assert_contains "publish job waits for all platform builds" "needs: [build, build-android]" "$WORKFLOW_YAML"
assert_contains "publish job only runs for version tags" "if: startsWith(github.ref, 'refs/tags/v')" "$WORKFLOW_YAML"
for pattern in \
  "Android-Chromebook-*.apk" \
  "Linux-Debian-ARM64-*.deb" \
  "Linux-Debian-x86_64-*.deb" \
  "Linux-RPM-ARM64-*.rpm" \
  "Linux-RPM-x86_64-*.rpm" \
  "macOS-Apple-Silicon-*.dmg" \
  "macOS-Intel-*.dmg" \
  "Windows-MSI-*.msi"; do
  assert_contains "publish job requires $pattern" "$pattern" "$WORKFLOW_YAML"
done

echo
echo "3. publishing flips the draft once, and marks it latest"
PUBLISH_COUNT="$(count_matches '-f draft=false')"
assert_eq "exactly one publish edit flips the draft" "1" "$PUBLISH_COUNT"
assert_contains "published release is marked latest with a typed form field" "-F make_latest=true" "$WORKFLOW_YAML"
assert_contains "publish is verified against releases/latest" "releases/latest" "$WORKFLOW_YAML"
assert_contains 're-run against an already-public release exits cleanly' '${RELEASE_ID:-}' "$WORKFLOW_YAML"


echo
echo "4. asset uploads only run for version tags"
TAG_GUARD_COUNT="$(count_matches "startsWith(github.ref, 'refs/tags/v')")"
assert_eq "tag guard on both upload steps, docker and publish jobs" "4" "$TAG_GUARD_COUNT"

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"
[ "$FAIL" -eq 0 ]

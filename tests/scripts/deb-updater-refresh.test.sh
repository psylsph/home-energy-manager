#!/bin/bash
# Tests for the deb postinst refresh of the Proxmox LXC updater.
#
# The in-container update command is installed by the first-party LXC
# installer, not by the package, so it would otherwise stay frozen at
# whatever installer version created the container — updater fixes
# shipped in later releases (like the #291 retry-on-404 hardening)
# would never reach existing containers. The deb now ships a copy of
# the installer and its postinst refreshes the updater when it finds
# one installed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONF="$REPO_ROOT/src-tauri/tauri.conf.json"
INSTALLER="$REPO_ROOT/scripts/proxmox/install.sh"
PACKAGED_PATH="/usr/share/givenergy-local/proxmox-install.sh"

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

conf_value() {
  python3 - "$CONF" "$1" <<'PY'
import json
import sys

path, pointer = sys.argv[1:3]
with open(path, encoding='utf-8') as handle:
    value = json.load(handle)
for part in pointer.split('.'):
    value = value.get(part) if isinstance(value, dict) else None
    if value is None:
        break
print(json.dumps(value) if value is not None else '')
PY
}

POSTINST="$(conf_value bundle.deb.postInstScript)"
POSTINST_BODY=""
if [ -n "$POSTINST" ] && [ "$POSTINST" != "null" ]; then
  POSTINST_CONTENT="$(cat "$REPO_ROOT/src-tauri/$POSTINST")"
fi

echo "tests/scripts/deb-updater-refresh.test.sh"
echo

echo "1. the deb ships the LXC installer and a postinst refresh script"
DEB_FILES_JSON="$(python3 -c "
import json
print(json.dumps(json.load(open('$CONF')).get('bundle', {}).get('deb', {}).get('files', {})))
")"
DEB_CONFIG_JSON="$(python3 -c "
import json
print(json.dumps(json.load(open('$CONF')).get('bundle', {}).get('deb', {})))
")"
assert_contains "deb ships install.sh at the packaged path" "$PACKAGED_PATH" "$DEB_FILES_JSON"
assert_contains "packaged copy comes from the live installer" "../scripts/proxmox/install.sh" "$DEB_FILES_JSON"
assert_contains "postinst script is configured" "postInstScript" "$DEB_CONFIG_JSON"

if [ -z "${POSTINST_CONTENT:-}" ]; then
  echo "  FAIL  postinst content readable"
  echo "        missing: $CONF bundle.deb.postInstScript"
  FAIL=$((FAIL + 1))
  echo
  echo "---------------------------------------"
  echo "Passed: $PASS    Failed: $FAIL"
  echo "---------------------------------------"
  exit 1
fi

echo
echo "2. the postinst only touches the HEM updater, and replaces it atomically"
assert_contains "acts only when dpkg configures the package" '[ "$1" != "configure" ]' "$POSTINST_CONTENT"
assert_contains "guards against clobbering an unrelated update command" 'psylsph/home-energy-manager' "$POSTINST_CONTENT"
assert_contains "refreshes the standard update path" "/usr/local/bin/update" "$POSTINST_CONTENT"
assert_contains "refreshes the legacy pre-0.71.6 path" "/usr/local/sbin/home-energy-manager-update" "$POSTINST_CONTENT"
assert_contains "replaces the updater via atomic rename" "mv -f" "$POSTINST_CONTENT"

echo
echo "3. running the postinst refreshes a stale HEM updater"
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/share/givenergy-local" "$STAGE/usr/local/bin" "$STAGE/usr/local/sbin"
cp "$INSTALLER" "$STAGE/share/givenergy-local/proxmox-install.sh"
printf '#!/bin/bash\n# some older updater copy\nprintf "old updater\\n"\nREPO="psylsph/home-energy-manager"\n' >"$STAGE/usr/local/bin/update"
chmod 0755 "$STAGE/usr/local/bin/update"
sed "s|/usr/local/bin|$STAGE/usr/local/bin|g; s|/usr/local/sbin|$STAGE/usr/local/sbin|g; s|$PACKAGED_PATH|$STAGE/share/givenergy-local/proxmox-install.sh|g" \
  "$REPO_ROOT/src-tauri/$POSTINST" >"$STAGE/postinst"
sh "$STAGE/postinst" configure 0.75.7 >"$STAGE/output.log" 2>&1
assert_eq "postinst exits successfully" "0" "$?"
assert_contains "reports the refresh" "refreshed the Proxmox update command" "$(cat "$STAGE/output.log")"
cmp -s "$STAGE/usr/local/bin/update" "$INSTALLER"
assert_eq "updater content refreshed to the packaged installer" "0" "$?"
[ -x "$STAGE/usr/local/bin/update" ]
assert_eq "updater stays executable" "0" "$?"

echo
echo "4. an unrelated update command is never touched"
printf '#!/bin/sh\necho "something else entirely"\n' >"$STAGE/usr/local/bin/update"
sh "$STAGE/postinst" configure 0.75.7 >/dev/null 2>&1
cmp -s "$STAGE/usr/local/bin/update" <(printf '#!/bin/sh\necho "something else entirely"\n')
assert_eq "unrelated update command left untouched" "0" "$?"

echo
echo "5. a container without an updater gets nothing"
rm -f "$STAGE/usr/local/bin/update"
sh "$STAGE/postinst" configure 0.75.7 >/dev/null 2>&1
assert_eq "no updater appears on plain deb installs" "no" "$([ -e "$STAGE/usr/local/bin/update" ] && echo yes || echo no)"

echo
echo "6. the legacy pre-0.71.6 path is refreshed too"
printf 'REPO="psylsph/home-energy-manager"\n# legacy copy\n' >"$STAGE/usr/local/sbin/home-energy-manager-update"
sh "$STAGE/postinst" configure 0.75.7 >/dev/null 2>&1
cmp -s "$STAGE/usr/local/sbin/home-energy-manager-update" "$INSTALLER"
assert_eq "legacy updater path refreshed" "0" "$?"

echo
echo "7. non-configure dpkg phases change nothing"
cp "$STAGE/share/givenergy-local/proxmox-install.sh" "$STAGE/pristine"
printf '#/old content with marker\nREPO="psylsph/home-energy-manager"\n' >"$STAGE/usr/local/bin/update"
sh "$STAGE/postinst" abort-upgrade >/dev/null 2>&1
cmp -s "$STAGE/usr/local/bin/update" <(printf '#/old content with marker\nREPO="psylsph/home-energy-manager"\n')
assert_eq "abort-upgrade leaves the updater alone" "0" "$?"

rm -rf "$STAGE"

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"
[ "$FAIL" -eq 0 ]
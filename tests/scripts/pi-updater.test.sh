#!/bin/bash
# Tests for the Raspberry Pi / Debian package updater and its systemd timer.
#
# Headless Pi users update by hand today (download the .deb, dpkg -i it,
# restart the service) and asked for something better (issue #315). The
# updater has to work out the install's own ExecStart/User rather than
# assuming them — a headless port other than 7337 and a data directory
# under a non-root user's home are both normal on a Pi — and it must never
# leave the service stopped or install an unverified package.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONF="$REPO_ROOT/src-tauri/tauri.conf.json"
UPDATER="$REPO_ROOT/scripts/pi/givenergy-local-update.sh"
SERVICE_UNIT="$REPO_ROOT/scripts/pi/systemd/givenergy-local-update.service"
TIMER_UNIT="$REPO_ROOT/scripts/pi/systemd/givenergy-local-update.timer"
PACKAGED_PATH="/usr/share/givenergy-local/givenergy-local-update.sh"
INSTALLED_CMD="/usr/bin/givenergy-local-update"
SERVICE_UNIT_PATH="/usr/lib/systemd/system/givenergy-local-update.service"
TIMER_UNIT_PATH="/usr/lib/systemd/system/givenergy-local-update.timer"

INSTALLED_VERSION="0.83.5"
TARGET_VERSION="0.83.6"
ASSET="Linux-Debian-ARM64-Home-Energy-Manager-v${TARGET_VERSION}.deb"
OLD_ASSET="Linux-Debian-ARM64-Home-Energy-Manager-v${INSTALLED_VERSION}.deb"

TMPROOT="$(mktemp -d)"
trap 'rm -rf "$TMPROOT"' EXIT
STAGE_N=0

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

assert_nonzero() {
  local label="$1" actual="$2"
  if [ "$actual" -ne 0 ]; then
    echo "  PASS  $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label"
    echo "        expected a non-zero exit status"
    FAIL=$((FAIL + 1))
  fi
}

# Order check: does $needle appear after $first in the command log?
assert_after() {
  local label="$1" first="$2" needle="$3"
  local first_line needle_line
  first_line="$(grep -n -F -- "$first" "$STAGE/commands.log" | head -1 | cut -d: -f1 || true)"
  needle_line="$(grep -n -F -- "$needle" "$STAGE/commands.log" | head -1 | cut -d: -f1 || true)"
  if [ -n "$first_line" ] && [ -n "$needle_line" ] && [ "$needle_line" -gt "$first_line" ]; then
    echo "  PASS  $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL  $label"
    echo "        expected '$needle' after '$first' (${first_line:-none} -> ${needle_line:-none})"
    FAIL=$((FAIL + 1))
  fi
}

make_mock() {
  local dir="$1" name="$2"
  shift 2
  cat >"$dir/$name"
  chmod +x "$dir/$name"
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
print(value if isinstance(value, str) else json.dumps(value) if value is not None else '')
PY
}

sha_of_deb() {
  printf 'fake deb for %s\n' "$1" | sha256sum | awk '{print $1}'
}

# Unit file the mocked `systemctl cat` serves, modelled on the unit in
# INSTALL.md. ${1:-} is an optional GIVENERGY_LOCAL_CONFIG_DIR.
write_units() {
  local config_dir="${1:-}"
  cat >"$STAGE/givenergy-local.service" <<EOF
[Unit]
Description=Home Energy Manager
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/givenergy-local --headless --port 8080
Restart=on-failure
User=pi
${config_dir:+Environment=GIVENERGY_LOCAL_CONFIG_DIR=$config_dir}

[Install]
WantedBy=multi-user.target
EOF
}

# Rewrite the latest-release fixture. $1 is the digest value ('' omits the
# field entirely, as a release that publishes no digest would).
write_latest_json() {
  local digest="$1"
  python3 - "$STAGE/fixtures/latest.json" "$TARGET_VERSION" "$ASSET" "$digest" <<'PY'
import json
import sys

path, version, asset, digest = sys.argv[1:5]
entry = {
    "name": asset,
    "browser_download_url": (
        "https://github.com/psylsph/home-energy-manager/releases/download/"
        f"v{version}/{asset}"
    ),
}
if digest:
    entry["digest"] = digest
with open(path, 'w', encoding='utf-8') as handle:
    json.dump({"tag_name": f"v{version}", "assets": [entry]}, handle, indent=2)
PY
}

# The installed version's release fixture, fetched only to keep the old
# package for rollback, so its digest is the real one for the fake payload.
write_old_json() {
  python3 - "$STAGE/fixtures/old.json" "$INSTALLED_VERSION" "$OLD_ASSET" "sha256:$(sha_of_deb "$OLD_ASSET")" <<'PY'
import json
import sys

path, version, asset, digest = sys.argv[1:5]
with open(path, 'w', encoding='utf-8') as handle:
    json.dump(
        {
            "tag_name": f"v{version}",
            "assets": [
                {
                    "name": asset,
                    "browser_download_url": (
                        "https://github.com/psylsph/home-energy-manager/releases/download/"
                        f"v{version}/{asset}"
                    ),
                    "digest": digest,
                }
            ],
        },
        handle,
        indent=2,
    )
PY
}

stage() {
  STAGE_N=$((STAGE_N + 1))
  STAGE="$TMPROOT/stage$STAGE_N"
  mkdir -p "$STAGE/bin" "$STAGE/fixtures" "$STAGE/backups" "$STAGE/home/pi/.givenergy-local"
  printf '{"readings":[]}' >"$STAGE/home/pi/.givenergy-local/settings.json"
  printf 'binary-db' >"$STAGE/home/pi/.givenergy-local/history.db"
  echo "$INSTALLED_VERSION" >"$STAGE/installed-version"
  : >"$STAGE/commands.log"
  write_units
  write_latest_json "sha256:$(sha_of_deb "$ASSET")"
  write_old_json

  make_mock "$STAGE/bin" id <<'EOF'
#!/bin/bash
[ "${1:-}" = "-u" ] && { echo "${HEM_TEST_UID:-0}"; exit 0; }
/usr/bin/id "$@"
EOF

  make_mock "$STAGE/bin" dpkg <<'EOF'
#!/bin/bash
printf 'dpkg %s\n' "$*" >>"$HEM_TEST_LOG"
case "${1:-}" in
  --print-architecture)
    if [ "${HEM_NO_DPKG:-0}" = "1" ]; then exit 127; fi
    echo "${HEM_TEST_ARCH:-arm64}"
    exit 0
    ;;
  --compare-versions)
    if [ "${3:-}" = "lt" ] && [ "$2" != "$4" ] \
      && [ "$(printf '%s\n%s\n' "$2" "$4" | sort -V | head -1)" = "$2" ]; then
      exit 0
    fi
    exit 1
    ;;
esac
exit 1
EOF

  make_mock "$STAGE/bin" dpkg-query <<'EOF'
#!/bin/bash
printf 'dpkg-query %s\n' "$*" >>"$HEM_TEST_LOG"
if [ "${1:-}" = "-W" ]; then
  cat "$HEM_STATE_DIR/installed-version"
  exit 0
fi
exit 1
EOF

  make_mock "$STAGE/bin" getent <<'EOF'
#!/bin/bash
printf 'getent %s\n' "$*" >>"$HEM_TEST_LOG"
if [ "${1:-}" = "passwd" ]; then
  printf '%s:x:1000:1000::%s:/bin/bash\n' "${2:-pi}" "${HEM_TEST_HOME:-/home/pi}"
  exit 0
fi
exit 1
EOF

  make_mock "$STAGE/bin" systemctl <<'EOF'
#!/bin/bash
printf 'systemctl %s\n' "$*" >>"$HEM_TEST_LOG"
case "${1:-}" in
  cat)
    if [ "${HEM_NO_UNIT:-0}" = "1" ]; then exit 1; fi
    if [ "${2:-}" = "givenergy-local.service" ]; then
      cat "$HEM_STATE_DIR/givenergy-local.service"
      exit 0
    fi
    exit 1
    ;;
  is-enabled) exit "${HEM_TIMER_ENABLED_EXIT:-1}" ;;
esac
exit 0
EOF

  make_mock "$STAGE/bin" apt-get <<'EOF'
#!/bin/bash
printf 'apt-get %s\n' "$*" >>"$HEM_TEST_LOG"
if [ "${HEM_APT_FAIL:-0}" = "1" ]; then exit 100; fi
last="${!#}"
case "$last" in
  *"$HEM_OLD_ASSET") echo "$HEM_INSTALLED_VERSION" >"$HEM_STATE_DIR/installed-version" ;;
  *) echo "$HEM_TARGET_VERSION" >"$HEM_STATE_DIR/installed-version" ;;
esac
exit 0
EOF

  make_mock "$STAGE/bin" curl <<'EOF'
#!/bin/bash
printf 'curl %s\n' "$*" >>"$HEM_TEST_LOG"
case "$*" in *--version*) exit 0 ;; esac
out=''
url=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  *releases/latest)
    cp "$HEM_STATE_DIR/fixtures/latest.json" "$out"
    ;;
  *releases/tags/*)
    cp "$HEM_STATE_DIR/fixtures/old.json" "$out"
    ;;
  *.deb)
    printf 'fake deb for %s\n' "${url##*/}" >"$out"
    ;;
  *'/api/status')
    if [ "${HEM_HEALTH_FAIL:-0}" = "1" ]; then exit 7; fi
    ;;
  *)
    echo "unexpected curl target: $url" >&2
    exit 1
    ;;
esac
exit 0
EOF
}

# Runs the updater with the staged mocks in front of the real tools.
run_updater() {
  PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_STATE_DIR="$STAGE" \
  HEM_BACKUP_DIR="$STAGE/backups" \
  HEM_TEST_HOME="$STAGE/home/pi" \
  HEM_INSTALLED_VERSION="$INSTALLED_VERSION" \
  HEM_TARGET_VERSION="$TARGET_VERSION" \
  HEM_OLD_ASSET="$OLD_ASSET" \
  HEM_TEST_ARCH="${HEM_TEST_ARCH:-arm64}" \
  "$UPDATER" "$@"
}

echo "tests/scripts/pi-updater.test.sh"
echo

echo "1. the deb ships the updater and both systemd units"
DEB_FILES_JSON="$(conf_value bundle.linux.deb.files)"
DEB_CONFIG_JSON="$(conf_value bundle.linux.deb)"
POSTINST="$(conf_value bundle.linux.deb.postInstallScript)"
assert_contains "deb ships the updater script" "$PACKAGED_PATH" "$DEB_FILES_JSON"
assert_contains "packaged updater comes from the live script" "../scripts/pi/givenergy-local-update.sh" "$DEB_FILES_JSON"
assert_contains "deb ships the update service unit" "$SERVICE_UNIT_PATH" "$DEB_FILES_JSON"
assert_contains "deb ships the update timer unit" "$TIMER_UNIT_PATH" "$DEB_FILES_JSON"
assert_contains "packaged units come from the live unit files" "../scripts/pi/systemd/givenergy-local-update.service" "$DEB_FILES_JSON"
assert_contains "postinst script is configured" "postInstallScript" "$DEB_CONFIG_JSON"

POSTINST_CONTENT=''
if [ -f "$REPO_ROOT/src-tauri/${POSTINST:-postinst-missing}" ]; then
  POSTINST_CONTENT="$(cat "$REPO_ROOT/src-tauri/$POSTINST")"
fi
assert_contains "postinst installs the command onto PATH" "$INSTALLED_CMD" "$POSTINST_CONTENT"
assert_contains "postinst makes the command executable" "install -m 0755" "$POSTINST_CONTENT"
assert_contains "postinst reloads systemd units" "daemon-reload" "$POSTINST_CONTENT"
assert_not_contains "postinst never enables the timer for the user" "enable --now givenergy-local-update.timer" "$POSTINST_CONTENT"

echo
echo "2. the update units are opt-in, weekly and never self-enabling"
SERVICE_CONTENT=''
TIMER_CONTENT=''
if [ -f "$SERVICE_UNIT" ] && [ -f "$TIMER_UNIT" ]; then
  SERVICE_CONTENT="$(cat "$SERVICE_UNIT")"
  TIMER_CONTENT="$(cat "$TIMER_UNIT")"
fi
assert_contains "service runs the updater" "ExecStart=$INSTALLED_CMD" "$SERVICE_CONTENT"
assert_not_contains "service does not enable itself" "[Install]" "$SERVICE_CONTENT"
assert_contains "timer fires weekly" "OnCalendar=weekly" "$TIMER_CONTENT"
assert_contains "timer spreads the load" "RandomizedDelaySec=" "$TIMER_CONTENT"
assert_contains "timer catches up missed runs" "Persistent=true" "$TIMER_CONTENT"
assert_contains "timer is what the user enables" "WantedBy=timers.target" "$TIMER_CONTENT"

if [ ! -f "$UPDATER" ]; then
  echo
  echo "  FAIL  updater exists at scripts/pi/givenergy-local-update.sh"
  FAIL=$((FAIL + 1))
  echo
  echo "---------------------------------------"
  echo "Passed: $PASS    Failed: $FAIL"
  echo "---------------------------------------"
  exit 1
fi

echo
echo "3. --help explains itself and changes nothing"
stage
RC=0
run_updater --help >"$STAGE/help.log" 2>&1 || RC=$?
assert_eq "--help exits successfully" "0" "$RC"
assert_contains "--help documents --check" "--check" "$(cat "$STAGE/help.log")"
assert_contains "--help documents --service" "--service" "$(cat "$STAGE/help.log")"
assert_eq "no commands ran" "" "$(cat "$STAGE/commands.log")"

echo
echo "4. a non-root run refuses before touching anything"
RC=0
HEM_TEST_UID=1000 run_updater >"$STAGE/nonroot.log" 2>&1 || RC=$?
assert_nonzero "non-root exits non-zero" "$RC"
assert_contains "non-root is told to use sudo" "sudo" "$(cat "$STAGE/nonroot.log")"
assert_eq "no commands ran" "" "$(cat "$STAGE/commands.log")"

echo
echo "5. --check reports an up-to-date install and changes nothing"
stage
echo "$TARGET_VERSION" >"$STAGE/installed-version"
RC=0
run_updater --check >"$STAGE/current.log" 2>&1 || RC=$?
assert_eq "up to date exits 0" "0" "$RC"
assert_contains "up to date is stated" "up to date" "$(cat "$STAGE/current.log")"
assert_not_contains "no package was downloaded" "$ASSET" "$(cat "$STAGE/commands.log")"

echo
echo "6. --check reports an available update with a distinct exit code"
stage
RC=0
run_updater --check >"$STAGE/check.log" 2>&1 || RC=$?
assert_eq "update available exits 10" "10" "$RC"
assert_contains "the newer version is named" "$TARGET_VERSION" "$(cat "$STAGE/check.log")"
assert_contains "the installed version is named" "$INSTALLED_VERSION" "$(cat "$STAGE/check.log")"
assert_not_contains "check does not install" "apt-get" "$(cat "$STAGE/commands.log")"
assert_not_contains "check does not touch the service" "systemctl stop" "$(cat "$STAGE/commands.log")"

echo
echo "7. the python3 JSON fallback parses releases too"
RC=0
HEM_JSON_TOOL=python3 run_updater --check >"$STAGE/check-py.log" 2>&1 || RC=$?
assert_eq "python3 path finds the update" "10" "$RC"
assert_contains "python3 path names the newer version" "$TARGET_VERSION" "$(cat "$STAGE/check-py.log")"

echo
echo "8. a full update stops, installs the verified package and restarts"
stage
RC=0
run_updater >"$STAGE/update.log" 2>&1 || RC=$?
assert_eq "update exits 0" "0" "$RC"
LOG="$(cat "$STAGE/commands.log")"
APT_LOG="$(grep -F 'apt-get' "$STAGE/commands.log" || true)"
assert_contains "the new release was downloaded" "$ASSET" "$LOG"
assert_contains "the package was installed from the download" "apt-get install -y" "$APT_LOG"
assert_after "the service stops before the install" "systemctl stop givenergy-local.service" "apt-get install -y"
assert_after "the service starts after the install" "apt-get install -y" "systemctl start givenergy-local.service"
assert_contains "the port comes from the unit's ExecStart" "127.0.0.1:8080/api/status" "$LOG"
assert_contains "the unit's User is honoured" "getent passwd pi" "$LOG"
assert_contains "the new version is reported" "$TARGET_VERSION" "$(cat "$STAGE/update.log")"
BACKUP_COUNT="$(find "$STAGE/backups" -name 'pre-update-*.tar.gz' | wc -l)"
assert_eq "the user's data was copied aside first" "1" "$BACKUP_COUNT"
assert_contains "the backup contains the data" "settings.json" "$(tar -tzf "$STAGE/backups"/pre-update-*.tar.gz)"
assert_contains "live data is left alone" '{"readings":[]}' "$(cat "$STAGE/home/pi/.givenergy-local/settings.json")"

echo
echo "9. --port overrides the port used for the health check"
stage
RC=0
run_updater --port 9443 >"$STAGE/port.log" 2>&1 || RC=$?
assert_eq "explicit port update exits 0" "0" "$RC"
assert_contains "the override is used" "127.0.0.1:9443/api/status" "$(cat "$STAGE/commands.log")"

echo
echo "10. an install newer than the latest release is left alone"
stage
echo "9.9.9" >"$STAGE/installed-version"
RC=0
run_updater >"$STAGE/newer.log" 2>&1 || RC=$?
assert_eq "newer install exits 0" "0" "$RC"
assert_contains "newer install is explained" "newer" "$(cat "$STAGE/newer.log")"
assert_not_contains "nothing was installed" "apt-get" "$(cat "$STAGE/commands.log")"

echo
echo "11. a release with no digest is never installed"
stage
write_latest_json ""
RC=0
run_updater >"$STAGE/nodigest.log" 2>&1 || RC=$?
assert_nonzero "missing digest exits non-zero" "$RC"
assert_contains "missing digest is explained" "digest" "$(cat "$STAGE/nodigest.log")"
assert_not_contains "nothing was installed" "apt-get" "$(cat "$STAGE/commands.log")"
assert_not_contains "the service was never stopped" "systemctl stop" "$(cat "$STAGE/commands.log")"

echo
echo "12. a digest mismatch stops the update before the service is touched"
stage
write_latest_json "sha256:$(printf 'tampered\n' | sha256sum | awk '{print $1}')"
RC=0
run_updater >"$STAGE/mismatch.log" 2>&1 || RC=$?
assert_nonzero "digest mismatch exits non-zero" "$RC"
assert_contains "digest mismatch is explained" "verification failed" "$(cat "$STAGE/mismatch.log")"
assert_not_contains "nothing was installed" "apt-get" "$(cat "$STAGE/commands.log")"
assert_not_contains "the service was never stopped" "systemctl stop" "$(cat "$STAGE/commands.log")"

echo
echo "13. a package that fails its health check is rolled back"
stage
RC=0
HEM_HEALTH_FAIL=1 run_updater >"$STAGE/rollback.log" 2>&1 || RC=$?
assert_nonzero "failed health check exits non-zero" "$RC"
LOG="$(cat "$STAGE/commands.log")"
APT_LOG="$(grep -F 'apt-get' "$STAGE/commands.log" || true)"
assert_contains "the previous package is reinstalled" "$OLD_ASSET" "$APT_LOG"
assert_contains "the rollback is explained" "restoring" "$(cat "$STAGE/rollback.log")"
assert_eq "the installed version is back to the old one" "$INSTALLED_VERSION" "$(cat "$STAGE/installed-version")"
assert_contains "the service ends up running" "systemctl start givenergy-local.service" "$LOG"
assert_contains "the rollback never overwrites live data" '{"readings":[]}' "$(cat "$STAGE/home/pi/.givenergy-local/settings.json")"

echo
echo "14. a failed package install restarts the service instead of leaving it stopped"
stage
RC=0
HEM_APT_FAIL=1 run_updater >"$STAGE/aptfail.log" 2>&1 || RC=$?
assert_nonzero "failed install exits non-zero" "$RC"
LOG="$(cat "$STAGE/commands.log")"
APT_LOG="$(grep -F 'apt-get' "$STAGE/commands.log" || true)"
assert_contains "the service is started again" "systemctl start givenergy-local.service" "$LOG"
assert_contains "the install failure is explained" "install failed" "$(cat "$STAGE/aptfail.log")"
assert_not_contains "no rollback install was attempted" "$OLD_ASSET" "$APT_LOG"

echo
echo "15. a data directory configured in the unit is the one backed up"
stage
write_units "$STAGE/var-lib/givenergy-local"
mkdir -p "$STAGE/var-lib/givenergy-local"
printf '{"readings":[1]}' >"$STAGE/var-lib/givenergy-local/settings.json"
RC=0
run_updater >"$STAGE/configdir.log" 2>&1 || RC=$?
assert_eq "configured data dir update exits 0" "0" "$RC"
assert_contains "the configured data dir was copied" "settings.json" "$(tar -tzf "$STAGE/backups"/pre-update-*.tar.gz)"
assert_contains "the configured data is the copy that was taken" '{"readings":[1]}' "$(tar -xzOf "$STAGE/backups"/pre-update-*.tar.gz '*/settings.json')"

echo
echo "16. without a unit a real update refuses, but --check still works"
stage
RC=0
HEM_NO_UNIT=1 run_updater >"$STAGE/nounit.log" 2>&1 || RC=$?
assert_nonzero "missing unit exits non-zero" "$RC"
assert_contains "missing unit is explained" "--service" "$(cat "$STAGE/nounit.log")"
assert_not_contains "nothing was installed" "apt-get" "$(cat "$STAGE/commands.log")"

stage
RC=0
HEM_NO_UNIT=1 run_updater --check >"$STAGE/nounit-check.log" 2>&1 || RC=$?
assert_eq "--check works without a unit" "10" "$RC"

echo
echo "17. --service selects a differently named unit"
stage
cp "$STAGE/givenergy-local.service" "$STAGE/custom-hem.service"
make_mock "$STAGE/bin" systemctl <<'EOF'
#!/bin/bash
printf 'systemctl %s\n' "$*" >>"$HEM_TEST_LOG"
case "${1:-}" in
  cat)
    [ "${2:-}" = "custom-hem.service" ] && { cat "$HEM_STATE_DIR/custom-hem.service"; exit 0; }
    exit 1
    ;;
esac
exit 0
EOF
RC=0
run_updater --service custom-hem.service >"$STAGE/custom.log" 2>&1 || RC=$?
assert_eq "custom unit update exits 0" "0" "$RC"
LOG="$(cat "$STAGE/commands.log")"
assert_contains "the custom unit was stopped" "systemctl stop custom-hem.service" "$LOG"
assert_contains "the custom unit was started" "systemctl start custom-hem.service" "$LOG"

echo
echo "18. only the three most recent data copies are kept"
stage
for stamp in 20260101T000000Z 20260102T000000Z 20260103T000000Z 20260104T000000Z; do
  printf 'old copy' >"$STAGE/backups/pre-update-$stamp.tar.gz"
done
RC=0
run_updater >"$STAGE/retention.log" 2>&1 || RC=$?
assert_eq "update with existing copies exits 0" "0" "$RC"
assert_eq "old copies are pruned to three" "3" "$(find "$STAGE/backups" -name 'pre-update-*.tar.gz' | wc -l)"
assert_eq "the oldest copy is gone" "no" "$([ -e "$STAGE/backups/pre-update-20260101T000000Z.tar.gz" ] && echo yes || echo no)"

echo
echo "19. a system without dpkg is told so instead of half-updating"
stage
RC=0
HEM_NO_DPKG=1 run_updater >"$STAGE/nodpkg.log" 2>&1 || RC=$?
assert_nonzero "missing dpkg exits non-zero" "$RC"
assert_contains "missing dpkg is explained" "Debian" "$(cat "$STAGE/nodpkg.log")"
assert_not_contains "nothing was installed" "apt-get" "$(cat "$STAGE/commands.log")"

echo
echo "20. an unsupported architecture is refused"
stage
RC=0
HEM_TEST_ARCH=armhf run_updater >"$STAGE/armhf.log" 2>&1 || RC=$?
assert_nonzero "unsupported arch exits non-zero" "$RC"
assert_contains "unsupported arch is explained" "arm64" "$(cat "$STAGE/armhf.log")"
assert_not_contains "nothing was installed" "apt-get" "$(cat "$STAGE/commands.log")"

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"
[ "$FAIL" -eq 0 ]

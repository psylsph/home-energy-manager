#!/bin/bash
# Tests for the first-party Proxmox LXC helper and in-container installer.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CREATE="$REPO_ROOT/scripts/proxmox/create-lxc.sh"

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

make_mock() {
  local dir="$1" name="$2"
  shift 2
  cat >"$dir/$name"
  chmod +x "$dir/$name"
}

stage_proxmox_mocks() {
  local root="$1"
  mkdir -p "$root/bin"
  : >"$root/commands.log"

  make_mock "$root/bin" id <<'EOF'
#!/bin/bash
[ "${1:-}" = "-u" ] && { echo 0; exit 0; }
/usr/bin/id "$@"
EOF

  make_mock "$root/bin" pvesh <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >>"$HEM_TEST_LOG"
echo 200
EOF

  make_mock "$root/bin" pvesm <<'EOF'
#!/bin/bash
printf 'pvesm %s\n' "$*" >>"$HEM_TEST_LOG"
case "$*" in
  *vztmpl*) printf 'Name Type Status Total Used Available %%\nlocal dir active 1 1 1 1%%\n' ;;
  *rootdir*) printf 'Name Type Status Total Used Available %%\nlocal-lvm lvmthin active 1 1 1 1%%\n' ;;
esac
EOF

  make_mock "$root/bin" pveam <<'EOF'
#!/bin/bash
printf 'pveam %s\n' "$*" >>"$HEM_TEST_LOG"
case "${1:-}" in
  available) echo 'system debian-13-standard_13.1-2_amd64.tar.zst' ;;
  list) echo 'Volid Format Type Size VMID' ;;
esac
EOF

  make_mock "$root/bin" pct <<'EOF'
#!/bin/bash
printf 'pct %s\n' "$*" >>"$HEM_TEST_LOG"
case "${1:-}" in
  status) exit 1 ;;
  exec)
    if [[ "$*" == *'hostname -I'* ]]; then echo '192.0.2.10'; fi
    exit 0
    ;;
esac
EOF

  make_mock "$root/bin" curl <<'EOF'
#!/bin/bash
printf 'curl %s\n' "$*" >>"$HEM_TEST_LOG"
out=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then out="$2"; shift 2; else shift; fi
done
if [ -n "$out" ]; then
  cp "$HEM_TEST_INSTALLER" "$out"
fi
EOF
}

run_create() {
  local stage="$1"
  PATH="$stage/bin:/usr/bin:/bin" \
    HEM_TEST_LOG="$stage/commands.log" \
    HEM_TEST_INSTALLER="$REPO_ROOT/scripts/proxmox/install.sh" \
    HEM_SCRIPT_REF="test-ref" \
    HEM_GATEWAY="${HEM_TEST_GATEWAY:-}" \
    bash "$CREATE" >"$stage/output.log" 2>&1
}

echo "tests/scripts/proxmox-lxc.test.sh"
echo
echo "1. creates an unprivileged Debian 13 LXC and runs the installer"
STAGE="$(mktemp -d)"
stage_proxmox_mocks "$STAGE"
set +e
run_create "$STAGE"
RC=$?
set -e
COMMANDS="$(cat "$STAGE/commands.log")"
CREATE_OUTPUT="$(cat "$STAGE/output.log")"
assert_eq "script exits successfully" "0" "$RC"
assert_contains "downloads Debian 13 template" "pveam download local debian-13-standard_13.1-2_amd64.tar.zst" "$COMMANDS"
assert_contains "creates unprivileged container" "pct create 200 local:vztmpl/debian-13-standard_13.1-2_amd64.tar.zst" "$COMMANDS"
assert_contains "uses unprivileged mode" "--unprivileged 1" "$COMMANDS"
assert_contains "uses four gigabyte root disk" "--rootfs local-lvm:4" "$COMMANDS"
assert_contains "uses bridged DHCP networking" "--net0 name=eth0,bridge=vmbr0,ip=dhcp" "$COMMANDS"
assert_contains "pushes in-container installer" "pct push 200" "$COMMANDS"
assert_contains "runs in-container installer" "pct exec 200 -- env HEM_PORT=7337 bash /root/home-energy-manager-install.sh" "$COMMANDS"
assert_contains "verifies the inspected installer copy" "Installer integrity verified" "$CREATE_OUTPUT"
rm -rf "$STAGE"

echo
echo "2. rejects network-option injection through the gateway override"
STAGE="$(mktemp -d)"
stage_proxmox_mocks "$STAGE"
set +e
HEM_TEST_GATEWAY='192.168.1.1,firewall=0' run_create "$STAGE"
RC=$?
set -e
COMMANDS="$(cat "$STAGE/commands.log")"
assert_eq "invalid gateway exits non-zero" "1" "$RC"
assert_not_contains "does not create a container" "pct create" "$COMMANDS"
rm -rf "$STAGE"

echo
echo "3. installs the architecture-matched release and creates the service"
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/bin" "$STAGE/root"
: >"$STAGE/commands.log"
printf 'fake deb package\n' >"$STAGE/deb-fixture"
DIGEST="$(sha256sum "$STAGE/deb-fixture" | cut -d' ' -f1)"

make_mock "$STAGE/bin" id <<'EOF'
#!/bin/bash
[ "${1:-}" = "-u" ] && { echo 0; exit 0; }
/usr/bin/id "$@"
EOF
make_mock "$STAGE/bin" dpkg <<'EOF'
#!/bin/bash
[ "${1:-}" = "--print-architecture" ] && { echo amd64; exit 0; }
exit 1
EOF
make_mock "$STAGE/bin" dpkg-query <<'EOF'
#!/bin/bash
if [ -n "${HEM_TEST_INSTALLED_VERSION:-}" ]; then
  printf '%s\n' "$HEM_TEST_INSTALLED_VERSION"
  exit 0
fi
exit 1
EOF
make_mock "$STAGE/bin" apt <<'EOF'
#!/bin/bash
printf 'apt %s\n' "$*" >>"$HEM_TEST_LOG"
exit 0
EOF
make_mock "$STAGE/bin" jq <<'EOF'
#!/bin/bash
field=''
asset_name=''
json_file=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -er) shift ;;
    --arg)
      [ "${2:-}" = "name" ] || exit 2
      asset_name="${3:-}"
      shift 3
      ;;
    .tag_name) field='tag_name'; shift ;;
    *browser_download_url*) field='browser_download_url'; shift ;;
    *digest*) field='digest'; shift ;;
    *) json_file="$1"; shift ;;
  esac
done
python3 - "$json_file" "$field" "$asset_name" <<'PY'
import json
import sys

path, field, asset_name = sys.argv[1:]
with open(path, encoding='utf-8') as handle:
    payload = json.load(handle)
if field == 'tag_name':
    value = payload.get('tag_name')
else:
    asset = next((item for item in payload.get('assets', []) if item.get('name') == asset_name), None)
    value = asset.get(field) if asset else None
if value is None:
    raise SystemExit(1)
print(value)
PY
EOF
make_mock "$STAGE/bin" systemctl <<'EOF'
#!/bin/bash
printf 'systemctl %s\n' "$*" >>"$HEM_TEST_LOG"
exit 0
EOF
make_mock "$STAGE/bin" curl <<'EOF'
#!/bin/bash
printf 'curl %s\n' "$*" >>"$HEM_TEST_LOG"
out=''
url=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    http*) url="$1"; shift ;;
    *) shift ;;
  esac
done
if [[ "$url" == *'/releases/latest' ]]; then
  printf '{"tag_name":"v1.2.3","assets":[{"name":"Linux-Debian-x86_64-Home-Energy-Manager-v1.2.3.deb","browser_download_url":"https://example.invalid/hem.deb","digest":"sha256:%s"}]}\n' "$HEM_TEST_DIGEST" >"$out"
elif [[ "$url" == *'/releases/tags/v1.2.2' ]]; then
  printf '{"tag_name":"v1.2.2","assets":[{"name":"Linux-Debian-x86_64-Home-Energy-Manager-v1.2.2.deb","browser_download_url":"https://example.invalid/hem-old.deb","digest":"sha256:%s"}]}\n' "$HEM_TEST_DIGEST" >"$out"
elif [ -n "$out" ]; then
  cp "$HEM_TEST_DEB" "$out"
elif [[ "$url" == *'/api/status' ]]; then
  if [ "${HEM_TEST_HEALTH_ALWAYS_FAIL:-0}" = "1" ]; then
    exit 22
  elif [ "${HEM_TEST_HEALTH_FAIL_ONCE:-0}" = "1" ]; then
    count="$(cat "$HEM_TEST_HEALTH_STATE" 2>/dev/null || echo 0)"
    count=$((count + 1))
    printf '%s\n' "$count" >"$HEM_TEST_HEALTH_STATE"
    [ "$count" -gt 1 ] || exit 22
  fi
  printf '{"ok":true}\n'
fi
EOF

set +e
PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_TEST_DEB="$STAGE/deb-fixture" \
  HEM_TEST_DIGEST="$DIGEST" \
  HEM_ROOT="$STAGE/root" \
  HEM_PORT=7444 \
  bash "$REPO_ROOT/scripts/proxmox/install.sh" >"$STAGE/output.log" 2>&1
RC=$?
set -e
if [ "$RC" -ne 0 ]; then
  sed 's/^/        installer: /' "$STAGE/output.log"
  sed 's/^/        command: /' "$STAGE/commands.log"
fi
COMMANDS="$(cat "$STAGE/commands.log")"
SERVICE="$(cat "$STAGE/root/etc/systemd/system/home-energy-manager.service" 2>/dev/null || true)"
assert_eq "installer exits successfully" "0" "$RC"
assert_contains "selects amd64 release asset" "Linux-Debian-x86_64-Home-Energy-Manager-v1.2.3.deb" "$COMMANDS"
assert_contains "installs downloaded package" "apt install -y" "$COMMANDS"
assert_contains "persists data outside package paths" "Environment=GIVENERGY_LOCAL_CONFIG_DIR=/var/lib/givenergy-local" "$SERVICE"
assert_contains "runs headless on configured port" "ExecStart=/usr/bin/givenergy-local --headless --port 7444" "$SERVICE"
assert_contains "isolates the service with a dynamic user" "DynamicUser=true" "$SERVICE"
assert_contains "assigns the persistent state directory" "StateDirectory=givenergy-local" "$SERVICE"
assert_contains "drops all Linux capabilities" "CapabilityBoundingSet=" "$SERVICE"
assert_contains "does not grant ambient capabilities" "AmbientCapabilities=" "$SERVICE"
assert_contains "enables service" "systemctl enable --now home-energy-manager.service" "$COMMANDS"
assert_contains "installs update command" "yes" "$([ -x "$STAGE/root/usr/local/sbin/home-energy-manager-update" ] && echo yes || echo no)"

echo
echo "4. update command is a no-op when the latest version is installed"
: >"$STAGE/commands.log"
set +e
PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_TEST_DEB="$STAGE/deb-fixture" \
  HEM_TEST_DIGEST="$DIGEST" \
  HEM_TEST_INSTALLED_VERSION="1.2.3" \
  HEM_ROOT="$STAGE/root" \
  bash "$STAGE/root/usr/local/sbin/home-energy-manager-update" >"$STAGE/update-output.log" 2>&1
UPDATE_RC=$?
set -e
UPDATE_COMMANDS="$(cat "$STAGE/commands.log")"
UPDATE_OUTPUT="$(cat "$STAGE/update-output.log")"
assert_eq "update exits successfully" "0" "$UPDATE_RC"
assert_contains "reports already current" "already installed" "$UPDATE_OUTPUT"
assert_not_contains "does not download the package" "https://example.invalid/hem.deb" "$UPDATE_COMMANDS"
assert_not_contains "does not reinstall the package" "apt install -y /tmp/" "$UPDATE_COMMANDS"

echo
echo "5. update stops the service and backs up persistent data"
mkdir -p "$STAGE/root/var/lib/givenergy-local"
printf '{"inverter_host":"192.0.2.20"}\n' >"$STAGE/root/var/lib/givenergy-local/settings.json"
mkdir -p "$STAGE/root/var/backups/home-energy-manager"
for stamp in 20260101T000000Z 20260201T000000Z 20260301T000000Z 20260401T000000Z; do
  : >"$STAGE/root/var/backups/home-energy-manager/pre-update-${stamp}.tar.gz"
done
: >"$STAGE/commands.log"
set +e
PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_TEST_DEB="$STAGE/deb-fixture" \
  HEM_TEST_DIGEST="$DIGEST" \
  HEM_TEST_INSTALLED_VERSION="1.2.2" \
  HEM_ROOT="$STAGE/root" \
  bash "$STAGE/root/usr/local/sbin/home-energy-manager-update" >"$STAGE/upgrade-output.log" 2>&1
UPGRADE_RC=$?
set -e
UPGRADE_COMMANDS="$(cat "$STAGE/commands.log")"
UPGRADED_SERVICE="$(cat "$STAGE/root/etc/systemd/system/home-energy-manager.service")"
BACKUPS=("$STAGE/root/var/backups/home-energy-manager"/pre-update-*.tar.gz)
assert_eq "upgrade exits successfully" "0" "$UPGRADE_RC"
assert_contains "stops service before replacing package" "systemctl stop home-energy-manager.service" "$UPGRADE_COMMANDS"
assert_contains "installs newer package" "apt install -y /tmp/" "$UPGRADE_COMMANDS"
assert_contains "creates pre-update data backup" "yes" "$([ -f "${BACKUPS[0]}" ] && echo yes || echo no)"
assert_contains "retains only three newest backups" "3" "${#BACKUPS[@]}"
assert_contains "preserves the configured port" "ExecStart=/usr/bin/givenergy-local --headless --port 7444" "$UPGRADED_SERVICE"

echo
echo "6. failed post-update health check restores the previous package"
: >"$STAGE/commands.log"
rm -f "$STAGE/health-state"
set +e
PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_TEST_DEB="$STAGE/deb-fixture" \
  HEM_TEST_DIGEST="$DIGEST" \
  HEM_TEST_INSTALLED_VERSION="1.2.2" \
  HEM_TEST_HEALTH_FAIL_ONCE=1 \
  HEM_TEST_HEALTH_STATE="$STAGE/health-state" \
  HEM_ROOT="$STAGE/root" \
  bash "$STAGE/root/usr/local/sbin/home-energy-manager-update" >"$STAGE/rollback-output.log" 2>&1
ROLLBACK_RC=$?
set -e
ROLLBACK_COMMANDS="$(cat "$STAGE/commands.log")"
ROLLBACK_OUTPUT="$(cat "$STAGE/rollback-output.log")"
assert_eq "failed update reports non-zero" "1" "$ROLLBACK_RC"
assert_contains "downloads previous release package" "Linux-Debian-x86_64-Home-Energy-Manager-v1.2.2.deb" "$ROLLBACK_COMMANDS"
assert_contains "reinstalls previous package with downgrade allowed" "apt install -y --allow-downgrades /tmp/" "$ROLLBACK_COMMANDS"
assert_contains "reports successful rollback" "Previous version restored" "$ROLLBACK_OUTPUT"

echo
echo "7. rejects a release package with a mismatched digest"
: >"$STAGE/commands.log"
set +e
PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_TEST_DEB="$STAGE/deb-fixture" \
  HEM_TEST_DIGEST="$(printf '0%.0s' {1..64})" \
  HEM_ROOT="$STAGE/bad-root" \
  bash "$REPO_ROOT/scripts/proxmox/install.sh" >"$STAGE/digest-output.log" 2>&1
DIGEST_RC=$?
set -e
DIGEST_OUTPUT="$(cat "$STAGE/digest-output.log")"
assert_eq "digest mismatch exits non-zero" "1" "$DIGEST_RC"
assert_contains "reports digest verification failure" "SHA-256 verification failed" "$DIGEST_OUTPUT"

echo
echo "8. failed first-install health check removes the failed installation"
: >"$STAGE/commands.log"
set +e
PATH="$STAGE/bin:/usr/bin:/bin" \
  HEM_TEST_LOG="$STAGE/commands.log" \
  HEM_TEST_DEB="$STAGE/deb-fixture" \
  HEM_TEST_DIGEST="$DIGEST" \
  HEM_TEST_HEALTH_ALWAYS_FAIL=1 \
  HEM_ROOT="$STAGE/fresh-root" \
  bash "$REPO_ROOT/scripts/proxmox/install.sh" >"$STAGE/fresh-failure-output.log" 2>&1
FRESH_FAILURE_RC=$?
set -e
FRESH_FAILURE_COMMANDS="$(cat "$STAGE/commands.log")"
assert_eq "failed first install reports non-zero" "1" "$FRESH_FAILURE_RC"
assert_contains "disables failed service" "systemctl disable --now home-energy-manager.service" "$FRESH_FAILURE_COMMANDS"
assert_contains "removes failed package" "apt remove -y home-energy-manager" "$FRESH_FAILURE_COMMANDS"
rm -rf "$STAGE"

echo
echo "---------------------------------------"
echo "Passed: $PASS    Failed: $FAIL"
echo "---------------------------------------"
[ "$FAIL" -eq 0 ]

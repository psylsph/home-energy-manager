#!/usr/bin/env bash
# Create a dedicated, unprivileged Proxmox LXC for Home Energy Manager.
# Run this script as root on a Proxmox VE host.
set -Eeuo pipefail

REPO="psylsph/home-energy-manager"
# Keep the bootstrap installer on the latest released tag. The tag is
# immutable, so this digest cannot silently drift when master changes.
SCRIPT_REF="${HEM_SCRIPT_REF:-v0.80.0}"
INSTALLER_SHA256="6d152c92c1fa1dcb61731066c70bac1c32935d1441402dd7beb5b364beb16409"
CTID="${HEM_CTID:-}"
HOSTNAME="${HEM_HOSTNAME:-home-energy-manager}"
CORES="${HEM_CORES:-1}"
MEMORY_MB="${HEM_MEMORY_MB:-1024}"
SWAP_MB="${HEM_SWAP_MB:-512}"
DISK_GB="${HEM_DISK_GB:-4}"
BRIDGE="${HEM_BRIDGE:-vmbr0}"
IP_CONFIG="${HEM_IP_CONFIG:-dhcp}"
GATEWAY="${HEM_GATEWAY:-}"
PORT="${HEM_PORT:-7337}"
TEMPLATE_STORAGE="${HEM_TEMPLATE_STORAGE:-}"
ROOTFS_STORAGE="${HEM_ROOTFS_STORAGE:-}"
KEEP_INSTALLER="${HEM_KEEP_INSTALLER:-0}"
CREATED_CTID=""
HEM_TMP=""

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

cleanup_on_exit() {
  local status="$?"
  trap - EXIT
  if [ "$status" -ne 0 ] && [ -n "$CREATED_CTID" ]; then
    printf 'Provisioning failed; removing container %s...\n' "$CREATED_CTID" >&2
    pct stop "$CREATED_CTID" || true
    pct destroy "$CREATED_CTID" --purge || true
  fi
  if [ -n "$HEM_TMP" ]; then
    rm -rf "$HEM_TMP"
  fi
  exit "$status"
}

trap cleanup_on_exit EXIT

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

require_uint() {
  local name="$1" value="$2" min="$3" max="$4"
  [[ "$value" =~ ^[0-9]+$ ]] || fail "$name must be an integer"
  ((value >= min && value <= max)) || fail "$name must be between $min and $max"
}

is_ipv4() {
  local address="$1" octet
  local -a octets
  IFS=. read -r -a octets <<<"$address"
  [ "${#octets[@]}" -eq 4 ] || return 1
  for octet in "${octets[@]}"; do
    [[ "$octet" =~ ^[0-9]{1,3}$ ]] || return 1
    ((10#$octet <= 255)) || return 1
  done
}

is_ipv4_cidr() {
  local value="$1" address prefix
  [[ "$value" == */* ]] || return 1
  address="${value%/*}"
  prefix="${value##*/}"
  is_ipv4 "$address" || return 1
  [[ "$prefix" =~ ^[0-9]{1,2}$ ]] && ((10#$prefix <= 32))
}

first_active_storage() {
  local content="$1"
  pvesm status -content "$content" | awk 'NR > 1 && $3 == "active" { print $1; exit }'
}

[ "$(id -u)" -eq 0 ] || fail "run this script as root on the Proxmox host"
for cmd in pct pveam pvesm pvesh curl awk sha256sum; do
  require_command "$cmd"
done

require_uint "HEM_CORES" "$CORES" 1 64
require_uint "HEM_MEMORY_MB" "$MEMORY_MB" 512 262144
require_uint "HEM_SWAP_MB" "$SWAP_MB" 0 262144
require_uint "HEM_DISK_GB" "$DISK_GB" 4 1024
require_uint "HEM_PORT" "$PORT" 1024 65535
[[ "$HOSTNAME" =~ ^[a-zA-Z0-9][a-zA-Z0-9.-]{0,62}$ ]] || fail "invalid HEM_HOSTNAME"
[[ "$BRIDGE" =~ ^[a-zA-Z0-9_.:-]+$ ]] || fail "invalid HEM_BRIDGE"
if [ "$IP_CONFIG" = "dhcp" ]; then
  [ -z "$GATEWAY" ] || fail "HEM_GATEWAY cannot be used with DHCP"
else
  is_ipv4_cidr "$IP_CONFIG" || fail "HEM_IP_CONFIG must be dhcp or a valid IPv4 CIDR"
  [ -z "$GATEWAY" ] || is_ipv4 "$GATEWAY" || fail "HEM_GATEWAY must be a valid IPv4 address"
fi

if [ -z "$CTID" ]; then
  CTID="$(pvesh get /cluster/nextid --output-format json | tr -d '"[:space:]')"
fi
require_uint "HEM_CTID" "$CTID" 100 999999999
if pct status "$CTID" >/dev/null 2>&1; then
  fail "container $CTID already exists"
fi

if [ -z "$TEMPLATE_STORAGE" ]; then
  TEMPLATE_STORAGE="$(first_active_storage vztmpl)"
fi
[ -n "$TEMPLATE_STORAGE" ] || fail "no active Proxmox storage supports container templates; set HEM_TEMPLATE_STORAGE"

if [ -z "$ROOTFS_STORAGE" ]; then
  ROOTFS_STORAGE="$(first_active_storage rootdir)"
fi
[ -n "$ROOTFS_STORAGE" ] || fail "no active Proxmox storage supports LXC root disks; set HEM_ROOTFS_STORAGE"

printf 'Refreshing Proxmox template catalogue...\n'
pveam update
TEMPLATE="$(pveam available --section system | awk '$2 ~ /^debian-13-standard_.*_amd64\.tar\.(zst|gz)$/ { print $2; exit }')"
[ -n "$TEMPLATE" ] || fail "no Debian 13 amd64 LXC template is available"

TEMPLATE_VOLUME="$(pveam list "$TEMPLATE_STORAGE" | awk -v template="$TEMPLATE" 'NR > 1 && index($1, template) { print $1; exit }')"
if [ -z "$TEMPLATE_VOLUME" ]; then
  printf 'Downloading %s...\n' "$TEMPLATE"
  pveam download "$TEMPLATE_STORAGE" "$TEMPLATE"
  TEMPLATE_VOLUME="${TEMPLATE_STORAGE}:vztmpl/${TEMPLATE}"
fi

NET0="name=eth0,bridge=${BRIDGE},ip=${IP_CONFIG}"
if [ -n "$GATEWAY" ]; then
  NET0+=",gw=${GATEWAY}"
fi

printf 'Creating unprivileged LXC %s...\n' "$CTID"
pct create "$CTID" "$TEMPLATE_VOLUME" \
  --arch amd64 \
  --ostype debian \
  --hostname "$HOSTNAME" \
  --cores "$CORES" \
  --memory "$MEMORY_MB" \
  --swap "$SWAP_MB" \
  --rootfs "${ROOTFS_STORAGE}:${DISK_GB}" \
  --net0 "$NET0" \
  --unprivileged 1 \
  --onboot 1 \
  --start 1
# Only the successful creator owns cleanup of this CTID. This avoids
# destroying a container another concurrent provisioning process created
# after a failed `pct create`.
CREATED_CTID="$CTID"

printf 'Waiting for network connectivity...\n'
network_ready=0
for _ in $(seq 1 30); do
  if pct exec "$CTID" -- getent hosts github.com >/dev/null 2>&1; then
    network_ready=1
    break
  fi
  sleep 2
done
[ "$network_ready" -eq 1 ] || fail "container started but could not reach github.com"

HEM_TMP="$(mktemp -d)"
INSTALLER="$HEM_TMP/home-energy-manager-install.sh"
INSTALLER_URL="https://raw.githubusercontent.com/${REPO}/${SCRIPT_REF}/scripts/proxmox/install.sh"
printf 'Downloading the in-container installer...\n'
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  -o "$INSTALLER" "$INSTALLER_URL"
[ -s "$INSTALLER" ] || fail "downloaded installer is empty"
ACTUAL_INSTALLER_SHA256="$(sha256sum "$INSTALLER" | awk '{ print $1 }')"
[ "$ACTUAL_INSTALLER_SHA256" = "$INSTALLER_SHA256" ] \
  || fail "installer integrity check failed; download create-lxc.sh again so both scripts are from the same revision"
printf 'Installer integrity verified.\n'

pct push "$CTID" "$INSTALLER" /root/home-energy-manager-install.sh --perms 0700
pct exec "$CTID" -- env HEM_PORT="$PORT" bash /root/home-energy-manager-install.sh
if [ "$KEEP_INSTALLER" != "1" ]; then
  pct exec "$CTID" -- rm -f /root/home-energy-manager-install.sh
fi

IP_ADDRESS="$(pct exec "$CTID" -- hostname -I 2>/dev/null | awk '{ print $1 }')"
printf '\nHome Energy Manager LXC created successfully.\n'
printf '  Container ID: %s\n' "$CTID"
printf '  Dashboard:    http://%s:%s\n' "${IP_ADDRESS:-<container-ip>}" "$PORT"
printf '  Update:       pct exec %s -- update\n' "$CTID"

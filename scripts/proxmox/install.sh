#!/usr/bin/env bash
# Install or update Home Energy Manager inside a Debian LXC.
set -Eeuo pipefail

REPO="psylsph/home-energy-manager"
DESTDIR="${HEM_ROOT:-}"
DATA_DIR="/var/lib/givenergy-local"
SERVICE_NAME="home-energy-manager.service"
SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}"
UPDATER_PATH="/usr/local/bin/update"
# Legacy path from <v0.72.0; removed on upgrade so stale copies don't linger.
OLD_UPDATER_PATH="/usr/local/sbin/home-energy-manager-update"
PORT_CONFIG_PATH="/etc/default/home-energy-manager"

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

root_path() {
  printf '%s%s' "$DESTDIR" "$1"
}

PORT="${HEM_PORT:-}"
if [ -z "$PORT" ] && [ -f "$(root_path "$PORT_CONFIG_PATH")" ]; then
  PORT="$(awk -F= '$1 == "HEM_PORT" { print $2; exit }' "$(root_path "$PORT_CONFIG_PATH")")"
fi
PORT="${PORT:-7337}"

[ "$(id -u)" -eq 0 ] || fail "run this installer as root inside the LXC"
if ! [[ "$PORT" =~ ^[0-9]+$ ]] || ((PORT < 1024 || PORT > 65535)); then
  fail "HEM_PORT must be between 1024 and 65535"
fi

case "$(dpkg --print-architecture)" in
  amd64) RELEASE_ARCH="x86_64" ;;
  arm64) RELEASE_ARCH="ARM64" ;;
  *) fail "unsupported architecture: $(dpkg --print-architecture) (supported: amd64, arm64)" ;;
esac

printf 'Installing download prerequisites...\n'
apt update
apt install -y ca-certificates curl jq

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
RELEASE_JSON="$TMPDIR/release.json"
API_URL="https://api.github.com/repos/${REPO}/releases/latest"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' \
  -o "$RELEASE_JSON" "$API_URL"

TAG="$(jq -er '.tag_name' "$RELEASE_JSON")" || fail "latest GitHub release has no tag"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "unexpected release tag: $TAG"
INSTALLED_VERSION="$(dpkg-query -W -f='${Version}' home-energy-manager 2>/dev/null || true)"
# Remove the legacy <v0.72.0 updater path if it exists from a prior install.
if [ -e "$(root_path "$OLD_UPDATER_PATH")" ]; then
  rm -f "$(root_path "$OLD_UPDATER_PATH")"
fi
if [ "$INSTALLED_VERSION" = "${TAG#v}" ]; then
  systemctl enable --now "$SERVICE_NAME"
  printf 'Home Energy Manager %s is already installed and running.\n' "$TAG"
  exit 0
fi
ASSET_NAME="Linux-Debian-${RELEASE_ARCH}-Home-Energy-Manager-${TAG}.deb"
ASSET_URL="$(jq -er --arg name "$ASSET_NAME" '.assets[] | select(.name == $name) | .browser_download_url' "$RELEASE_JSON")" \
  || fail "release $TAG does not contain $ASSET_NAME"
ASSET_DIGEST="$(jq -er --arg name "$ASSET_NAME" '.assets[] | select(.name == $name) | .digest' "$RELEASE_JSON")" \
  || fail "release $TAG does not publish a digest for $ASSET_NAME"
[[ "$ASSET_DIGEST" =~ ^sha256:[0-9a-fA-F]{64}$ ]] || fail "release digest is not a SHA-256 value"

DEB_PATH="$TMPDIR/$ASSET_NAME"
printf 'Downloading %s...\n' "$ASSET_NAME"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  -o "$DEB_PATH" "$ASSET_URL"
EXPECTED_SHA256="${ASSET_DIGEST#sha256:}"
ACTUAL_SHA256="$(sha256sum "$DEB_PATH" | awk '{ print $1 }')"
[ "${ACTUAL_SHA256,,}" = "${EXPECTED_SHA256,,}" ] || fail "SHA-256 verification failed for $ASSET_NAME"

OLD_DEB_PATH=""
if [ -n "$INSTALLED_VERSION" ]; then
  [[ "$INSTALLED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || fail "installed package version cannot be mapped to a release: $INSTALLED_VERSION"
  OLD_TAG="v${INSTALLED_VERSION}"
  OLD_ASSET_NAME="Linux-Debian-${RELEASE_ARCH}-Home-Energy-Manager-${OLD_TAG}.deb"
  OLD_RELEASE_JSON="$TMPDIR/old-release.json"
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    -o "$OLD_RELEASE_JSON" \
    "https://api.github.com/repos/${REPO}/releases/tags/${OLD_TAG}"
  OLD_ASSET_URL="$(jq -er --arg name "$OLD_ASSET_NAME" '.assets[] | select(.name == $name) | .browser_download_url' "$OLD_RELEASE_JSON")" \
    || fail "cannot retain previous package: release $OLD_TAG does not contain $OLD_ASSET_NAME"
  OLD_ASSET_DIGEST="$(jq -er --arg name "$OLD_ASSET_NAME" '.assets[] | select(.name == $name) | .digest' "$OLD_RELEASE_JSON")" \
    || fail "cannot retain previous package: $OLD_ASSET_NAME has no digest"
  [[ "$OLD_ASSET_DIGEST" =~ ^sha256:[0-9a-fA-F]{64}$ ]] || fail "previous package digest is not SHA-256"
  OLD_DEB_PATH="$TMPDIR/$OLD_ASSET_NAME"
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    -o "$OLD_DEB_PATH" "$OLD_ASSET_URL"
  OLD_ACTUAL_SHA256="$(sha256sum "$OLD_DEB_PATH" | awk '{ print $1 }')"
  [ "${OLD_ACTUAL_SHA256,,}" = "${OLD_ASSET_DIGEST#sha256:}" ] \
    || fail "SHA-256 verification failed for retained package $OLD_ASSET_NAME"
fi

if [ -n "$INSTALLED_VERSION" ]; then
  printf 'Stopping Home Energy Manager and backing up persistent data...\n'
  systemctl stop "$SERVICE_NAME"
  BACKUP_DIR="$(root_path /var/backups/home-energy-manager)"
  install -d -m 0700 "$BACKUP_DIR"
  if [ -d "$(root_path "$DATA_DIR")" ]; then
    BACKUP_PATH="$BACKUP_DIR/pre-update-$(date -u +%Y%m%dT%H%M%SZ).tar.gz"
    tar -C "$(dirname "$(root_path "$DATA_DIR")")" -czf "$BACKUP_PATH" "$(basename "$DATA_DIR")"
    mapfile -t BACKUPS < <(printf '%s\n' "$BACKUP_DIR"/pre-update-*.tar.gz | sort -r)
    if [ "${#BACKUPS[@]}" -gt 3 ]; then
      rm -- "${BACKUPS[@]:3}"
    fi
  fi
fi

check_health() {
  curl --fail --silent --show-error \
    --retry 15 --retry-connrefused --retry-delay 1 --max-time 5 \
    "http://127.0.0.1:${PORT}/api/status" >/dev/null
}

rollback_update() {
  local reason="$1"
  printf 'Update failed (%s); restoring Home Energy Manager %s...\n' "$reason" "$INSTALLED_VERSION" >&2
  systemctl stop "$SERVICE_NAME" || true
  if ! apt install -y --allow-downgrades "$OLD_DEB_PATH"; then
    fail "update and package rollback both failed; data backup remains at ${BACKUP_PATH:-<not-created>}"
  fi
  if [ -n "${BACKUP_PATH:-}" ] && [ -f "$BACKUP_PATH" ]; then
    rm -rf "$(root_path "$DATA_DIR")"
    tar -C "$(dirname "$(root_path "$DATA_DIR")")" -xzf "$BACKUP_PATH"
  fi
  systemctl daemon-reload
  systemctl enable --now "$SERVICE_NAME"
  check_health || fail "previous package was restored but its health check failed"
  printf 'Previous version restored successfully.\n' >&2
  exit 1
}

cleanup_failed_install() {
  local reason="$1"
  systemctl disable --now "$SERVICE_NAME" || true
  rm -f "$(root_path "$SERVICE_PATH")" \
    "$(root_path "$UPDATER_PATH")" \
    "$(root_path "$OLD_UPDATER_PATH")" \
    "$(root_path "$PORT_CONFIG_PATH")"
  systemctl daemon-reload
  apt remove -y home-energy-manager \
    || fail "$reason; automatic package cleanup also failed"
  fail "$reason; the failed package and service were removed"
}

printf 'Installing Home Energy Manager %s...\n' "$TAG"
if ! apt install -y "$DEB_PATH"; then
  if [ -n "$INSTALLED_VERSION" ]; then
    rollback_update "package installation failed"
  fi
  cleanup_failed_install "package installation failed"
fi

install -d -m 0700 "$(root_path "$DATA_DIR")"
install -d -m 0755 "$(dirname "$(root_path "$PORT_CONFIG_PATH")")"
printf 'HEM_PORT=%s\n' "$PORT" >"$(root_path "$PORT_CONFIG_PATH")"
chmod 0600 "$(root_path "$PORT_CONFIG_PATH")"
install -d -m 0755 "$(dirname "$(root_path "$SERVICE_PATH")")"
cat >"$(root_path "$SERVICE_PATH")" <<EOF
[Unit]
Description=Home Energy Manager
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
DynamicUser=true
StateDirectory=givenergy-local
StateDirectoryMode=0700
UMask=0077
Environment=GIVENERGY_LOCAL_CONFIG_DIR=$DATA_DIR
ExecStart=/usr/bin/givenergy-local --headless --port $PORT
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
CapabilityBoundingSet=
AmbientCapabilities=
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict
ReadWritePaths=$DATA_DIR

[Install]
WantedBy=multi-user.target
EOF

install -d -m 0755 "$(dirname "$(root_path "$UPDATER_PATH")")"
if [ "$(readlink -f "$0")" != "$(readlink -m "$(root_path "$UPDATER_PATH")")" ]; then
  install -m 0755 "$0" "$(root_path "$UPDATER_PATH")"
fi

systemctl daemon-reload
systemctl enable --now "$SERVICE_NAME"

printf 'Checking the local API...\n'
if ! check_health; then
  if [ -n "$INSTALLED_VERSION" ]; then
    rollback_update "health check failed"
  fi
  cleanup_failed_install "Home Energy Manager did not pass its health check"
fi

printf 'Home Energy Manager %s is running on port %s.\n' "$TAG" "$PORT"

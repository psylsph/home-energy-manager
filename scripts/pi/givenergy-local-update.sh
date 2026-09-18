#!/usr/bin/env bash
#
# Update a Debian / Raspberry Pi OS install of Home Energy Manager from the
# project's GitHub releases. Installed as /usr/bin/givenergy-local-update by
# the .deb, together with an opt-in weekly timer (see INSTALL.md).
#
# The app itself cannot do this: installing a package and restarting a
# systemd unit both need root, and a process cannot supervise its own
# replacement, so the updater lives outside it (issue #315).
set -Eeuo pipefail

REPO="psylsph/home-energy-manager"
PACKAGE="home-energy-manager"
DEFAULT_PORT=7337
# Units this updater will stop and restart, most specific first. Anything
# else has to be named explicitly with --service.
SERVICE_CANDIDATES=(givenergy-local.service home-energy-manager.service)
# Where the pre-update copy of the user's data is kept. Overridable so tests
# (and anyone wanting the copy elsewhere) don't need /var/backups.
BACKUP_DIR="${HEM_BACKUP_DIR:-/var/backups/givenergy-local}"
KEEP_BACKUPS=3

CHECK_ONLY=0
SERVICE=""
PORT=""

usage() {
  cat <<'EOF'
Usage: givenergy-local-update [options]

Update a Debian / Raspberry Pi OS install of Home Energy Manager to the
latest GitHub release. Needs root: the package install and the service
restart both do.

Options:
  --check            Report whether a newer release exists and change
                     nothing. Exits 0 when up to date, 10 when an update
                     is available, 1 on any error.
  --service <unit>   systemd unit to stop and restart. Defaults to
                     givenergy-local.service, then
                     home-energy-manager.service.
  --port <port>      Port for the post-update health check. Defaults to
                     the --port in the unit's ExecStart, then 7337.
  -h, --help         Show this help.

Settings and history are not part of the package and are never replaced by
an update. A copy is taken to /var/backups/givenergy-local before each
update (the newest three are kept) so a downgrade is possible by hand.
EOF
}

note() {
  printf '%s\n' "$*" >&2
}

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) CHECK_ONLY=1 ;;
    --service) SERVICE="${2:-}"; [ -n "$SERVICE" ] || fail "--service needs a unit name"; shift ;;
    --service=*) SERVICE="${1#--service=}" ;;
    --port) PORT="${2:-}"; [ -n "$PORT" ] || fail "--port needs a number"; shift ;;
    --port=*) PORT="${1#--port=}" ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option: $1 (try --help)" ;;
  esac
  shift
done

if [ -n "$PORT" ] && ! [[ "$PORT" =~ ^[0-9]+$ ]]; then
  fail "--port must be a number, got: $PORT"
fi

# Checking needs no privileges, but anything that touches the install does.
# Refuse before running a single other command so a mistaken unprivileged
# run cannot download, stop or replace anything.
if [ "$CHECK_ONLY" != "1" ] && [ "$(id -u)" -ne 0 ]; then
  fail "run this updater as root, for example: sudo givenergy-local-update"
fi

command -v dpkg >/dev/null 2>&1 \
  || fail "this updater replaces a Debian package, but dpkg is not available. On Debian or Raspberry Pi OS install the .deb as documented in INSTALL.md; other distributions are not supported yet."
command -v curl >/dev/null 2>&1 \
  || fail "this updater needs curl; install it with: sudo apt-get install curl"
ARCH="$(dpkg --print-architecture 2>/dev/null || true)"
case "$ARCH" in
  arm64) RELEASE_ARCH="ARM64" ;;
  amd64) RELEASE_ARCH="x86_64" ;;
  '') fail "could not determine the package architecture; is this a Debian-based system?" ;;
  *) fail "unsupported architecture: $ARCH (Home Energy Manager publishes arm64 and x86_64 packages only)" ;;
esac

# jq is the nicer dependency, but Raspberry Pi OS ships python3 out of the
# box and jq is not installed by default, so accept either.
JSON_TOOL="${HEM_JSON_TOOL:-}"
if [ -z "$JSON_TOOL" ]; then
  if command -v jq >/dev/null 2>&1; then
    JSON_TOOL=jq
  elif command -v python3 >/dev/null 2>&1; then
    JSON_TOOL=python3
  else
    fail "reading release metadata needs jq or python3; install one, for example: sudo apt-get install jq"
  fi
fi

# A release can be listed as latest a moment before its asset URLs come
# alive on GitHub's CDN, so a fresh download can 404 transiently (issue
# #291). curl only retries what it considers transient, and a 404 is not
# one, so opt into retrying every error class when curl supports it.
CURL_RETRY=(--retry 5 --retry-delay 5)
if curl --retry-all-errors --version >/dev/null 2>&1; then
  CURL_RETRY+=(--retry-all-errors)
fi

release_tag() {
  local json="$1"
  case "$JSON_TOOL" in
    jq) jq -er '.tag_name' "$json" ;;
    python3)
      python3 - "$json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding='utf-8') as handle:
    print(json.load(handle)["tag_name"])
PY
      ;;
  esac
}

# Field of one named asset in a release payload; fails if absent.
asset_field() {
  local json="$1" asset="$2" field="$3"
  case "$JSON_TOOL" in
    jq) jq -er --arg name "$asset" --arg field "$field" \
      '.assets[] | select(.name == $name) | .[$field]' "$json" ;;
    python3)
      python3 - "$json" "$asset" "$field" <<'PY'
import json
import sys

path, asset, field = sys.argv[1:4]
with open(path, encoding='utf-8') as handle:
    release = json.load(handle)
for entry in release.get("assets", []):
    if entry.get("name") == asset:
        value = entry.get(field)
        if isinstance(value, str):
            print(value)
            raise SystemExit(0)
        raise SystemExit(1)
raise SystemExit(1)
PY
      ;;
  esac
}

fetch_release() {
  local url="$1" out="$2"
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    "${CURL_RETRY[@]}" \
    -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    -o "$out" "$url"
}

WORK="$(mktemp -d)"
SERVICE_STOPPED=0

on_exit() {
  local status=$?
  trap - EXIT
  # Never leave the app stopped, whatever went wrong.
  if [ "$SERVICE_STOPPED" = "1" ] && [ -n "$SERVICE" ]; then
    note "Restarting $SERVICE after an unexpected failure..."
    systemctl start "$SERVICE" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORK"
  exit "$status"
}
trap on_exit EXIT

INSTALLED="$(dpkg-query -W -f='${Version}' "$PACKAGE" 2>/dev/null || true)"
[ -n "$INSTALLED" ] \
  || fail "$PACKAGE is not installed as a Debian package, so there is nothing to update. Install the .deb as documented in INSTALL.md."

LATEST_JSON="$WORK/latest.json"
if ! fetch_release "https://api.github.com/repos/${REPO}/releases/latest" "$LATEST_JSON"; then
  fail "could not read the latest release from GitHub; check the network connection and try again"
fi
TAG="$(release_tag "$LATEST_JSON" || true)"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "unexpected release tag: ${TAG:-<none>}"
TARGET="${TAG#v}"

if [ "$INSTALLED" = "$TARGET" ]; then
  if [ "$CHECK_ONLY" = "1" ]; then
    printf 'Home Energy Manager %s is up to date (latest release %s).\n' "$INSTALLED" "$TAG"
  else
    printf 'Home Energy Manager %s is already up to date.\n' "$INSTALLED"
  fi
  exit 0
fi

if ! dpkg --compare-versions "$INSTALLED" lt "$TARGET"; then
  # A local build or a newer release than the one GitHub calls latest.
  printf 'Home Energy Manager %s is newer than the latest release (%s); leaving it alone.\n' "$INSTALLED" "$TAG"
  exit 0
fi

if [ "$CHECK_ONLY" = "1" ]; then
  printf 'Home Energy Manager %s is installed; %s is available.\n' "$INSTALLED" "$TAG"
  exit 10
fi

# ---- everything below this point changes the install ----

if [ -z "$SERVICE" ]; then
  for candidate in "${SERVICE_CANDIDATES[@]}"; do
    if systemctl cat "$candidate" >/dev/null 2>&1; then
      SERVICE="$candidate"
      break
    fi
  done
fi
[ -n "$SERVICE" ] \
  || fail "could not find a Home Energy Manager systemd unit; pass --service <unit> naming the unit that runs givenergy-local"
systemctl cat "$SERVICE" >/dev/null 2>&1 \
  || fail "systemd has no unit called $SERVICE"
UNIT_TEXT="$(systemctl cat "$SERVICE")"

EXEC_LINE="$(printf '%s\n' "$UNIT_TEXT" | grep -m1 '^ExecStart=' || true)"
if [ -z "$PORT" ]; then
  UNIT_PORT=""
  WANT_PORT_NEXT=0
  for token in ${EXEC_LINE#ExecStart=}; do
    case "$token" in
      --port=*) UNIT_PORT="${token#--port=}"; break ;;
      --port) WANT_PORT_NEXT=1; continue ;;
    esac
    if [ "$WANT_PORT_NEXT" = "1" ]; then
      UNIT_PORT="$token"
      break
    fi
  done
  if [ -n "$UNIT_PORT" ] && [[ "$UNIT_PORT" =~ ^[0-9]+$ ]]; then
    PORT="$UNIT_PORT"
  else
    PORT="$DEFAULT_PORT"
  fi
fi

# The service user decides where settings and history live for a plain
# --headless install: <home>/.givenergy-local. A unit that sets
# GIVENERGY_LOCAL_CONFIG_DIR (as the LXC one does) says so explicitly, and
# that wins.
UNIT_CONFIG_DIR="$(printf '%s\n' "$UNIT_TEXT" | grep -m1 '^Environment=GIVENERGY_LOCAL_CONFIG_DIR=' \
  | sed 's/^Environment=GIVENERGY_LOCAL_CONFIG_DIR=//' || true)"
if [ -n "$UNIT_CONFIG_DIR" ]; then
  DATA_DIR="$UNIT_CONFIG_DIR"
else
  SERVICE_USER="$(printf '%s\n' "$UNIT_TEXT" | grep -m1 '^User=' | sed 's/^User=//' || true)"
  SERVICE_USER="${SERVICE_USER:-root}"
  SERVICE_HOME="$(getent passwd "$SERVICE_USER" | cut -d: -f6 || true)"
  DATA_DIR="${SERVICE_HOME:-/root}/.givenergy-local"
fi

# Copy the data aside before touching the package. It is not replaced by an
# install, but a newer database schema can leave the previous version unable
# to read it, so the copy is what makes a manual downgrade survivable. It is
# never restored automatically: the live directory is the source of truth.
copy_data_aside() {
  if [ ! -d "$DATA_DIR" ]; then
    note "No data directory at $DATA_DIR yet; nothing to copy aside."
    return 0
  fi
  install -d -m 0700 "$BACKUP_DIR" \
    || fail "could not create $BACKUP_DIR"
  local stamp path
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  path="$BACKUP_DIR/pre-update-$stamp.tar.gz"
  tar -C "$(dirname "$DATA_DIR")" -czf "$path" "$(basename "$DATA_DIR")" \
    || fail "could not copy $DATA_DIR aside to $path; nothing has been changed"
  mapfile -t copies < <(find "$BACKUP_DIR" -maxdepth 1 -name 'pre-update-*.tar.gz' | sort -r)
  if [ "${#copies[@]}" -gt "$KEEP_BACKUPS" ]; then
    rm -f -- "${copies[@]:KEEP_BACKUPS}"
  fi
  note "Copied $DATA_DIR to $path"
}

ASSET_NAME="Linux-Debian-${RELEASE_ARCH}-Home-Energy-Manager-${TAG}.deb"
ASSET_URL="$(asset_field "$LATEST_JSON" "$ASSET_NAME" browser_download_url || true)"
[ -n "$ASSET_URL" ] || fail "release $TAG has no $ASSET_NAME asset"
ASSET_DIGEST="$(asset_field "$LATEST_JSON" "$ASSET_NAME" digest || true)"
[ -n "$ASSET_DIGEST" ] \
  || fail "release $TAG publishes no digest for $ASSET_NAME, so the download cannot be verified"
[[ "$ASSET_DIGEST" =~ ^sha256:[0-9a-fA-F]{64}$ ]] \
  || fail "release digest for $ASSET_NAME is not a SHA-256 value: $ASSET_DIGEST"

download_verified() {
  local url="$1" digest="$2" dest="$3" actual
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    "${CURL_RETRY[@]}" -o "$dest" "$url" \
    || return 1
  actual="$(sha256sum "$dest" | awk '{print $1}')"
  if [ "${actual,,}" != "${digest#sha256:}" ]; then
    return 1
  fi
}

NEW_DEB="$WORK/$ASSET_NAME"
note "Downloading $ASSET_NAME..."
download_verified "$ASSET_URL" "$ASSET_DIGEST" "$NEW_DEB" \
  || fail "download or SHA-256 verification failed for $ASSET_NAME; nothing has been changed"

# Keep the currently installed release on hand so a package that installs
# but does not come up can be replaced. Missing historical assets are not
# fatal: the update still works, it just cannot be rolled back by this run.
OLD_DEB=""
OLD_ASSET_NAME="Linux-Debian-${RELEASE_ARCH}-Home-Energy-Manager-v${INSTALLED}.deb"
if [[ "$INSTALLED" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  OLD_JSON="$WORK/old.json"
  if fetch_release "https://api.github.com/repos/${REPO}/releases/tags/v${INSTALLED}" "$OLD_JSON"; then
    OLD_URL="$(asset_field "$OLD_JSON" "$OLD_ASSET_NAME" browser_download_url || true)"
    OLD_DIGEST="$(asset_field "$OLD_JSON" "$OLD_ASSET_NAME" digest || true)"
    if [ -n "$OLD_URL" ] && [[ "$OLD_DIGEST" =~ ^sha256:[0-9a-fA-F]{64}$ ]]; then
      if download_verified "$OLD_URL" "$OLD_DIGEST" "$WORK/$OLD_ASSET_NAME"; then
        OLD_DEB="$WORK/$OLD_ASSET_NAME"
      fi
    fi
  fi
fi
if [ -z "$OLD_DEB" ]; then
  note "Could not stage a copy of $INSTALLED, so a failed update cannot be rolled back automatically."
fi

copy_data_aside

check_health() {
  curl --fail --silent --show-error \
    --retry 15 --retry-connrefused --retry-delay 1 --max-time 5 \
    "http://127.0.0.1:${PORT}/api/status" >/dev/null
}

start_service() {
  systemctl daemon-reload >/dev/null 2>&1 || true
  if ! systemctl start "$SERVICE"; then
    SERVICE_STOPPED=1
    fail "could not start $SERVICE"
  fi
  SERVICE_STOPPED=0
}

roll_back() {
  local reason="$1"
  note "Update failed ($reason); restoring Home Energy Manager $INSTALLED..."
  [ -n "$OLD_DEB" ] \
    || fail "the new version did not start and no copy of $INSTALLED could be staged; the package is installed but $SERVICE is not running. Settings and history are untouched in $DATA_DIR."
  systemctl stop "$SERVICE" >/dev/null 2>&1 || true
  SERVICE_STOPPED=1
  if ! apt-get install -y --allow-downgrades "$OLD_DEB"; then
    SERVICE_STOPPED=1
    fail "update and rollback both failed; settings and history are untouched in $DATA_DIR"
  fi
  start_service
  if ! check_health; then
    fail "restored $INSTALLED but it did not pass its health check; settings and history are untouched in $DATA_DIR"
  fi
  note "Home Energy Manager $INSTALLED is running again."
  exit 1
}

note "Stopping $SERVICE..."
systemctl stop "$SERVICE" || fail "could not stop $SERVICE"
SERVICE_STOPPED=1

note "Installing Home Energy Manager $TARGET..."
if ! apt-get install -y "$NEW_DEB"; then
  # The previous package is normally still in place when apt fails; get the
  # app running again rather than attempting a downgrade install.
  start_service
  fail "package install failed; the previously installed version should still be in place and $SERVICE has been started again"
fi

start_service

if ! check_health; then
  roll_back "the new version did not answer on port $PORT"
fi

printf 'Home Energy Manager %s is installed and running on port %s (was %s).\n' \
  "$TARGET" "$PORT" "$INSTALLED"

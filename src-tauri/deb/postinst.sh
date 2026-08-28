#!/bin/sh
# Refresh the in-container Proxmox LXC updater from this package.
#
# The LXC update command (/usr/local/bin/update, and the pre-0.71.6
# /usr/local/sbin/home-energy-manager-update) is installed by the
# first-party LXC installer, not by this package, so without this it
# would stay frozen at whatever installer version created the container
# and never receive updater fixes shipped in later releases (issue #291:
# the retry-on-404 hardening could never reach existing containers).
# Plain .deb installs (desktops, servers) don't have the file and are
# skipped entirely.

if [ "$1" != "configure" ]; then
  exit 0
fi

PACKAGED=/usr/share/givenergy-local/proxmox-install.sh
[ -f "$PACKAGED" ] || exit 0

for UPDATER in /usr/local/bin/update /usr/local/sbin/home-energy-manager-update; do
  # Remove any temp file left by an earlier refresh that was interrupted
  # mid-copy, whether or not this run refreshes anything.
  rm -f "${UPDATER}.tmp"
  # Only refresh files the first-party installer wrote — never clobber an
  # unrelated command that happens to share the name. -I so a binary that
  # happens to embed the marker string isn't treated as ours.
  if [ -f "$UPDATER" ] && grep -Iq 'psylsph/home-energy-manager' "$UPDATER" 2>/dev/null; then
    # Copy then rename: a currently-running `update` keeps reading its
    # own inode and finishes cleanly instead of seeing a truncated script.
    if cp "$PACKAGED" "${UPDATER}.tmp" && chmod 0755 "${UPDATER}.tmp"; then
      mv -f "${UPDATER}.tmp" "$UPDATER"
      echo "home-energy-manager: refreshed the Proxmox update command."
    else
      rm -f "${UPDATER}.tmp"
    fi
  fi
done

exit 0
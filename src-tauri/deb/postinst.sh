#!/bin/sh
# Refresh the update commands this package ships from the packaged copies.
#
# Two different updaters live outside the package's own file list:
#
#  * the in-container Proxmox LXC updater (/usr/local/bin/update, and the
#    pre-0.71.6 /usr/local/sbin/home-energy-manager-update), installed by the
#    first-party LXC installer. Without this it would stay frozen at whatever
#    installer version created the container and never receive updater fixes
#    shipped in later releases (issue #291: the retry-on-404 hardening could
#    never reach existing containers).
#
#  * the headless .deb updater (/usr/bin/givenergy-local-update), installed
#    here from /usr/share because a file mapped straight into the package
#    cannot be relied on to keep its executable bit, and refreshed on every
#    upgrade so updater fixes keep reaching existing installs (issue #315).
#
# Both are opt-in to run; installing the package only puts them in place.
# The systemd units are shipped in place and are deliberately never enabled.

if [ "$1" != "configure" ]; then
  exit 0
fi

PACKAGED=/usr/share/givenergy-local/proxmox-install.sh
if [ -f "$PACKAGED" ]; then
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
fi

PACKAGED_PI=/usr/share/givenergy-local/givenergy-local-update.sh
PI_UPDATER=/usr/bin/givenergy-local-update
if [ -f "$PACKAGED_PI" ]; then
  rm -f "${PI_UPDATER}.tmp"
  # Only ever replace our own command, or create it where nothing is
  # installed yet.
  if [ ! -e "$PI_UPDATER" ] || grep -Iq 'psylsph/home-energy-manager' "$PI_UPDATER" 2>/dev/null; then
    if install -m 0755 "$PACKAGED_PI" "${PI_UPDATER}.tmp"; then
      mv -f "${PI_UPDATER}.tmp" "$PI_UPDATER"
      echo "home-energy-manager: installed the givenergy-local-update command."
    else
      rm -f "${PI_UPDATER}.tmp"
    fi
  fi
fi

# Let systemd notice units this package adds or replaces. Best effort: not
# every install runs systemd, and one that does not must not fail here.
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

exit 0

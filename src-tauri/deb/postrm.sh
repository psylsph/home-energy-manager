#!/bin/sh
# Remove the headless .deb updater that postinst installs onto PATH.
#
# /usr/bin/givenergy-local-update is not itself a package file — it is
# copied there from /usr/share so it keeps its executable bit — so dpkg will
# not remove it when the package goes. Skipped on upgrade, where the new
# package's postinst puts the command straight back.

case "$1" in
  remove|purge)
    PI_UPDATER=/usr/bin/givenergy-local-update
    # Only remove our own command: -I so a binary that happens to embed the
    # marker string isn't treated as ours.
    if [ -f "$PI_UPDATER" ] && grep -Iq 'psylsph/home-energy-manager' "$PI_UPDATER" 2>/dev/null; then
      rm -f "$PI_UPDATER"
    fi
    rm -f "${PI_UPDATER}.tmp"
    if command -v systemctl >/dev/null 2>&1; then
      systemctl daemon-reload >/dev/null 2>&1 || true
    fi
    ;;
esac

exit 0

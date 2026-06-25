#!/bin/bash
# Launch Home Energy Manager with WebKitGTK's GPU renderer disabled.
#
# Workaround for Raspberry Pi (and other Linux GPU stacks) where the
# default DMA-BUF + accelerated compositing path either fails to first
# paint or comes up corrupted. See:
#   - https://github.com/tauri-apps/tauri/issues/13885
#   - https://github.com/tauri-apps/tauri/issues/9394
#   - https://bugs.webkit.org/show_bug.cgi?id=261874
#   - https://bugs.launchpad.net/bugs/2037015  (Pi 4 banding fix landed in
#     webkit2gtk 2.44.2-0ubuntu0.24.04.2; Pi 5's VideoCore VII is newer
#     and may not be covered)
#
# The env vars must be set BEFORE GTK / WebKitGTK initialise, which means
# before the givenergy-local process starts. Setting them inside the app
# is too late — Tauri's setup hook runs after the runtime has already
# taken its pick. Hence the wrapper.
#
# Each var is set only when unset, so you can still override per-launch
# (e.g. RENDERER_DEBUG=1 ./scripts/run-with-software-renderer.sh).
#
# Usage:
#   ./scripts/run-with-software-renderer.sh                  # GUI window
#   ./scripts/run-with-software-renderer.sh --headless       # headless server
#   ./scripts/run-with-software-renderer.sh -- --port 8080   # extra args
#
# Or install once and call from anywhere:
#   sudo install -m 0755 scripts/run-with-software-renderer.sh \
#     /usr/local/bin/givenergy-local-safe
#   givenergy-local-safe                       # GUI
#   givenergy-local-safe --headless            # headless
set -euo pipefail

# 1. Force WebKitGTK to skip the DMA-BUF GPU renderer and fall back to
#    the legacy CPU framebuffer path. This is the upstream-endorsed
#    first-line fix (Igalia, WebKit Bugzilla #261874).
export WEBKIT_DISABLE_DMABUF_RENDERER="${WEBKIT_DISABLE_DMABUF_RENDERER:-1}"

# 2. Disable accelerated compositing. On X11 the DMA-BUF flag alone
#    misses the `AcceleratedSurfaceDMABuf was unable to construct a
#    complete framebuffer` failure mode — both together is the
#    community-tested combination (Tolaria, yaak, khoj/pipali #44).
export WEBKIT_DISABLE_COMPOSITING_MODE="${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"

# 3. Belt-and-braces: tell GLX/EGL to prefer a software renderer. Only
#    set if the user hasn't already chosen a backend — if you've set
#    GDK_BACKEND=wayland deliberately, leave it alone.
if [ -z "${GDK_BACKEND:-}" ]; then
  # Pi's default Raspberry Pi OS desktop is Wayland; X11 fallback is
  # more battle-tested with WebKitGTK's software path. If Wayland is
  # the active session we keep it (don't second-guess the user); on
  # X11 we let WebKitGTK pick.
  if [ "${XDG_SESSION_TYPE:-}" = "x11" ]; then
    export GDK_BACKEND=x11
  fi
fi

# Locate the installed binary. Falls back to PATH if /usr/bin/givenergy-local
# is missing (e.g. running from a hand-built artifact).
if [ -x /usr/bin/givenergy-local ]; then
  BINARY=/usr/bin/givenergy-local
elif command -v givenergy-local >/dev/null 2>&1; then
  BINARY="$(command -v givenergy-local)"
else
  echo "givenergy-local not found. Install the .deb from the Releases page first:" >&2
  echo "  https://github.com/psylsph/home-energy-manager/releases/latest" >&2
  exit 127
fi

echo "Launching $BINARY with software WebKit renderer"
echo "  WEBKIT_DISABLE_DMABUF_RENDERER=${WEBKIT_DISABLE_DMABUF_RENDERER}"
echo "  WEBKIT_DISABLE_COMPOSITING_MODE=${WEBKIT_DISABLE_COMPOSITING_MODE}"
echo "  GDK_BACKEND=${GDK_BACKEND:-<unset>}"
echo

exec "$BINARY" "$@"

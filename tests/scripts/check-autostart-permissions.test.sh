#!/bin/bash
# Regression test for issue #215: every IPC command used by the desktop
# Start on Login flow must be granted by the main-window capability.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CAPABILITY="$REPO_ROOT/src-tauri/capabilities/default.json"
PERMISSIONS_DIR="$REPO_ROOT/src-tauri/permissions"

python3 - "$CAPABILITY" "$PERMISSIONS_DIR" <<'PY'
import json
import pathlib
import sys
import tomllib

capability_path = pathlib.Path(sys.argv[1])
permissions_dir = pathlib.Path(sys.argv[2])
capability = json.loads(capability_path.read_text(encoding="utf-8"))
granted = set(capability.get("permissions", []))

required_plugin_permissions = {
    "autostart:allow-enable",
    "autostart:allow-disable",
}
missing_plugin = sorted(required_plugin_permissions - granted)
if missing_plugin:
    raise SystemExit(
        "FAIL: main-window capability is missing explicit autostart permissions: "
        + ", ".join(missing_plugin)
    )

custom_identifier = "allow-autostart-fallback"
if custom_identifier not in granted:
    raise SystemExit(
        f"FAIL: main-window capability does not grant {custom_identifier}"
    )

permission_files = list(permissions_dir.glob("*.toml")) if permissions_dir.exists() else []
permissions = []
for path in permission_files:
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    permissions.extend(document.get("permission", []))

custom_permissions = [
    permission
    for permission in permissions
    if permission.get("identifier") == custom_identifier
]
if not custom_permissions:
    raise SystemExit(
        f"FAIL: no application permission defines {custom_identifier}"
    )
allowed_commands = {
    command
    for permission in custom_permissions
    for command in permission.get("commands", {}).get("allow", [])
}
if "autostart_fallback" not in allowed_commands:
    raise SystemExit(
        "FAIL: application permission does not allow autostart_fallback"
    )

print("PASS: Start on Login IPC permissions are explicitly granted")
PY

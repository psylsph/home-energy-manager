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
import re
import sys

capability_path = pathlib.Path(sys.argv[1])
permissions_dir = pathlib.Path(sys.argv[2])
capability = json.loads(capability_path.read_text(encoding="utf-8"))
granted = set(capability.get("permissions", []))

# Release builds replace the bundled app URL with the embedded Axum server at
# 127.0.0.1:<configured port>. Tauri treats that page as a remote origin, so
# listing commands alone is not enough: the capability must also cover that
# loopback origin or every invoke is rejected by the runtime ACL.
remote_urls = set(capability.get("remote", {}).get("urls", []))
required_remote_url = "http://127.0.0.1:*"
if required_remote_url not in remote_urls:
    raise SystemExit(
        "FAIL: main-window capability does not grant the production loopback "
        f"origin ({required_remote_url})"
    )

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
    # Ubuntu 22.04 ships Python 3.10, before stdlib tomllib. These generated
    # Tauri permission files use a deliberately tiny TOML shape, so extract
    # only the identifier and allow-list this contract needs instead of
    # adding a package dependency just for a smoke test.
    content = path.read_text(encoding="utf-8")
    for block in re.split(r"(?m)^\s*\[\[permission\]\]\s*$", content)[1:]:
        identifier = re.search(r'(?m)^\s*identifier\s*=\s*"([^"]+)"', block)
        commands = re.search(
            r"(?ms)^\s*\[permission\.commands\]\s*$\s*(.*?)(?=^\s*\[|\Z)",
            block,
        )
        allow = re.search(
            r"(?m)^\s*allow\s*=\s*\[(.*?)\]",
            commands.group(1) if commands else "",
        )
        permissions.append(
            {
                "identifier": identifier.group(1) if identifier else "",
                "commands": {
                    "allow": re.findall(r'"([^"]+)"', allow.group(1)) if allow else []
                },
            }
        )

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

print("PASS: Start on Login IPC permissions and loopback origin are explicitly granted")
PY

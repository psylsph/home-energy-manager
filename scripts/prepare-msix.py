#!/usr/bin/env python3
"""Stage the Tauri Windows application for an unsigned Microsoft Store MSIX."""

from __future__ import annotations

import argparse
import html
import os
import re
import shutil
from pathlib import Path

ASSET_NAMES = (
    "StoreLogo.png",
    "Square44x44Logo.png",
    "Square71x71Logo.png",
    "Square150x150Logo.png",
)
TARGET_ARCHITECTURES = {
    "x86_64": "x64",
    "aarch64": "arm64",
    "i686": "x86",
}


def msix_version(version: str) -> str:
    """Convert semver (optionally prefixed with v) to MSIX's four-part version.

    Microsoft Store packages cannot use zero as the first version component.
    Offset the semver major by one so the current 0.x release line maps to a
    valid 1.x Store version, while preserving ordering when the project reaches
    1.x (which will map to 2.x). The fourth component remains the Store-owned
    zero for the normal three-component release input.
    """
    core = re.split(r"[-+]", version.removeprefix("v"), maxsplit=1)[0]
    parts = core.split(".")
    if len(parts) < 3 or len(parts) > 4 or any(not part.isdigit() for part in parts):
        raise ValueError(f"version must contain 3 or 4 numeric components: {version}")
    numbers = [int(part) for part in parts]
    if any(number > 65535 for number in numbers):
        raise ValueError(f"MSIX version components must be at most 65535: {version}")
    if len(numbers) == 4 and numbers[3] != 0:
        raise ValueError("MSIX version fourth component is reserved for Store: " + version)
    numbers[0] += 1
    if numbers[0] > 65535:
        raise ValueError(f"MSIX version major component cannot be offset: {version}")
    return ".".join(str(number) for number in (*numbers, 0, 0, 0, 0)[:4])


def target_architecture(target: str) -> str:
    prefix = target.split("-", maxsplit=1)[0]
    try:
        return TARGET_ARCHITECTURES[prefix]
    except KeyError as error:
        raise ValueError(f"unsupported Windows target: {target}") from error


def render_manifest(template: str, replacements: dict[str, str]) -> str:
    rendered = template
    for token, value in replacements.items():
        rendered = rendered.replace(f"__{token}__", html.escape(value, quote=True))
    unresolved = sorted(set(re.findall(r"__[A-Z_]+__", rendered)))
    if unresolved:
        raise ValueError(f"unresolved manifest tokens: {', '.join(unresolved)}")
    return rendered


def remove_staging_path(path: Path) -> None:
    """Remove a stale staging directory, file, or symlink without following it."""
    if not os.path.lexists(path):
        return
    if path.is_symlink() or not path.is_dir():
        path.unlink()
    else:
        shutil.rmtree(path)


def stage_msix(
    repo_root: Path,
    target: str,
    version: str,
    identity_name: str,
    publisher: str,
    publisher_display_name: str,
) -> tuple[Path, Path]:
    release_dir = repo_root / "src-tauri" / "target" / target / "release"
    executable = release_dir / "givenergy-local.exe"
    dist_dir = repo_root / "dist"
    template_path = repo_root / "src-tauri" / "msix" / "Package.appxmanifest"
    icons_dir = repo_root / "src-tauri" / "icons"

    required = (executable, dist_dir, template_path)
    missing = [str(path) for path in required if not path.exists()]
    missing.extend(str(icons_dir / name) for name in ASSET_NAMES if not (icons_dir / name).is_file())
    if missing:
        raise FileNotFoundError("required MSIX inputs are missing: " + ", ".join(missing))

    bundle_dir = release_dir / "bundle" / "msix"
    stage_dir = bundle_dir / "stage"
    remove_staging_path(stage_dir)
    (stage_dir / "Assets").mkdir(parents=True)

    shutil.copy2(executable, stage_dir / executable.name)
    shutil.copytree(dist_dir, stage_dir / "dist")
    for dll in release_dir.glob("*.dll"):
        shutil.copy2(dll, stage_dir / dll.name)
    for name in ASSET_NAMES:
        shutil.copy2(icons_dir / name, stage_dir / "Assets" / name)

    manifest = render_manifest(
        template_path.read_text(encoding="utf-8"),
        {
            "IDENTITY_NAME": identity_name,
            "PUBLISHER": publisher,
            "PUBLISHER_DISPLAY_NAME": publisher_display_name,
            "VERSION": msix_version(version),
            "ARCHITECTURE": target_architecture(target),
        },
    )
    (stage_dir / "Package.appxmanifest").write_text(manifest, encoding="utf-8")

    output = bundle_dir / f"HomeEnergyManager_{msix_version(version)}_{target_architecture(target)}.msix"
    return stage_dir, output


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()

    stage_dir, output = stage_msix(
        args.repo_root.resolve(),
        args.target,
        args.version,
        os.environ.get("MSIX_IDENTITY_NAME") or "com.givenergy.local",
        os.environ.get("MSIX_PUBLISHER") or "CN=Home Energy Manager",
        os.environ.get("MSIX_PUBLISHER_DISPLAY_NAME") or "Stuart Harding",
    )
    print(f"MSIX_STAGE={stage_dir}")
    print(f"MSIX_OUTPUT={output}")


if __name__ == "__main__":
    main()

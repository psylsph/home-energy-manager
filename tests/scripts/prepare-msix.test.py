#!/usr/bin/env python3
"""Regression tests for the WinApp CLI MSIX staging inputs."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "prepare-msix.py"
spec = importlib.util.spec_from_file_location("prepare_msix", SCRIPT_PATH)
assert spec and spec.loader
prepare_msix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare_msix)


class PrepareMsixTests(unittest.TestCase):
    def test_msix_version_accepts_release_semver(self) -> None:
        self.assertEqual(prepare_msix.msix_version("v0.66.3"), "0.66.3.0")
        self.assertEqual(prepare_msix.msix_version("1.2.3-beta.1"), "1.2.3.0")

    def test_msix_version_rejects_invalid_or_oversized_components(self) -> None:
        for version in ("1.2", "1.two.3", "1.2.3.4.5", "1.2.65536"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                prepare_msix.msix_version(version)

    def test_stages_executable_frontend_assets_dlls_and_store_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = "x86_64-pc-windows-msvc"
            release = root / "src-tauri" / "target" / target / "release"
            release.mkdir(parents=True)
            (release / "givenergy-local.exe").write_bytes(b"exe")
            (release / "WebView2Loader.dll").write_bytes(b"dll")
            (root / "dist").mkdir()
            (root / "dist" / "index.html").write_text("frontend", encoding="utf-8")

            template_dir = root / "src-tauri" / "msix"
            template_dir.mkdir(parents=True)
            template_dir.joinpath("Package.appxmanifest").write_text(
                (REPO_ROOT / "src-tauri" / "msix" / "Package.appxmanifest").read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            icons = root / "src-tauri" / "icons"
            icons.mkdir()
            for name in prepare_msix.ASSET_NAMES:
                (icons / name).write_bytes(b"png")

            stage, output = prepare_msix.stage_msix(
                root,
                target,
                "0.66.3",
                "Store.Identity",
                "CN=Publisher & Test",
                "Publisher <Test>",
            )

            self.assertEqual(output.name, "HomeEnergyManager_0.66.3.0_x64.msix")
            self.assertEqual((stage / "givenergy-local.exe").read_bytes(), b"exe")
            self.assertEqual((stage / "WebView2Loader.dll").read_bytes(), b"dll")
            self.assertEqual((stage / "dist" / "index.html").read_text(encoding="utf-8"), "frontend")
            self.assertTrue(all((stage / "Assets" / name).is_file() for name in prepare_msix.ASSET_NAMES))

            manifest_path = stage / "Package.appxmanifest"
            manifest_text = manifest_path.read_text(encoding="utf-8")
            self.assertNotIn("__", manifest_text)
            root_element = ET.parse(manifest_path).getroot()
            namespace = {"m": "http://schemas.microsoft.com/appx/manifest/foundation/windows10"}
            identity = root_element.find("m:Identity", namespace)
            self.assertIsNotNone(identity)
            assert identity is not None
            self.assertEqual(identity.attrib["Name"], "Store.Identity")
            self.assertEqual(identity.attrib["Publisher"], "CN=Publisher & Test")
            self.assertEqual(identity.attrib["Version"], "0.66.3.0")
            self.assertEqual(identity.attrib["ProcessorArchitecture"], "x64")

    def test_release_workflow_builds_an_unsigned_store_package(self) -> None:
        workflow = (REPO_ROOT / ".github" / "workflows" / "build.yml").read_text(encoding="utf-8")
        self.assertIn("uses: microsoft/setup-WinAppCli@v0.1", workflow)
        self.assertIn("version: v0.4.0", workflow)
        self.assertGreaterEqual(workflow.count("continue-on-error: true"), 2)
        self.assertIn("scripts\\prepare-msix.py", workflow)
        self.assertIn("winapp pack $stageDir", workflow)
        self.assertNotIn("winapp pack $stageDir --cert", workflow)
        self.assertIn('if ext_lower == ".msix":', workflow)
        self.assertIn('".msix", ".exe"', workflow)

    def test_manual_smoke_workflow_uploads_the_msix_without_creating_a_release(self) -> None:
        workflow = (REPO_ROOT / ".github" / "workflows" / "msix-smoke.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("cargo tauri build --target x86_64-pc-windows-msvc --no-bundle", workflow)
        self.assertIn("winapp pack $stageDir", workflow)
        self.assertIn("uses: actions/upload-artifact@v6", workflow)
        self.assertNotIn("action-gh-release", workflow)
        self.assertNotIn("continue-on-error", workflow)

    def test_missing_executable_fails_before_creating_a_package_layout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(FileNotFoundError, "givenergy-local.exe"):
                prepare_msix.stage_msix(
                    Path(tmp),
                    "x86_64-pc-windows-msvc",
                    "0.66.3",
                    "Store.Identity",
                    "CN=Publisher",
                    "Publisher",
                )


if __name__ == "__main__":
    unittest.main()

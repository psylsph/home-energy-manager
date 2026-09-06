#!/usr/bin/env python3
"""Pin CI coverage for code whose behaviour depends on the host OS."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github" / "workflows" / "ci.yml").read_text()


def require(fragment: str, description: str) -> None:
    if fragment not in WORKFLOW:
        raise AssertionError(f"CI must {description}: missing {fragment!r}")


require(
    "os: [ubuntu-22.04, macos-latest, windows-latest]",
    "run Rust tests natively on Linux, macOS, and Windows",
)
require("runs-on: ${{ matrix.os }}", "use the native platform-test matrix")
require(
    "if: matrix.os == 'ubuntu-22.04'",
    "install WebKit/GTK build dependencies only on Linux",
)
require("run: cargo test", "execute the Rust suite on every matrix platform")
require("run: npm test", "execute frontend and repository contract tests")

print("Platform CI coverage checks passed")

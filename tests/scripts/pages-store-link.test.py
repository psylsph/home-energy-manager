#!/usr/bin/env python3
"""Regression test for the Microsoft Store link on the GitHub Pages hero."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PAGE = ROOT / "docs" / "index.html"
STORE_URL = "https://apps.microsoft.com/detail/9mtm4bf2htbt?ocid=webpdpshare"

html = PAGE.read_text(encoding="utf-8")

download_position = html.index(">Download Free</a>")
store_position = html.index(f'href="{STORE_URL}"')
warning_position = html.index('<div class="windows-warning">')

assert download_position < store_position < warning_position, (
    "Microsoft Store link must appear below Download Free and before the "
    "Windows security notice"
)
assert "get Home Energy Manager from the Microsoft Store" in html
assert f'href="{STORE_URL}" target="_blank" rel="noopener noreferrer"' in html

print("PASS: Microsoft Store link is present in the requested hero position")

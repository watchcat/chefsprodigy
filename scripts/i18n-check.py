#!/usr/bin/env python3
"""Validate the dependency-free frontend message bundles."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
I18N = ROOT / "frontend" / "dist" / "i18n"
LANGUAGES = ("en", "ru", "nl")


def read_bundle(language: str) -> dict[str, str]:
    path = I18N / f"{language}.json"
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"{path}: {error}") from error
    if not isinstance(value, dict) or not all(isinstance(key, str) and isinstance(text, str) and text for key, text in value.items()):
        raise SystemExit(f"{path}: expected a non-empty string value for every string key")
    return value


def main() -> int:
    bundles = {language: read_bundle(language) for language in LANGUAGES}
    expected = set(bundles["en"])
    failures: list[str] = []
    for language, bundle in bundles.items():
        missing = sorted(expected - set(bundle))
        extra = sorted(set(bundle) - expected)
        if missing:
            failures.append(f"{language}: missing keys: {', '.join(missing)}")
        if extra:
            failures.append(f"{language}: unexpected keys: {', '.join(extra)}")

    source = (ROOT / "frontend" / "dist" / "app.js").read_text(encoding="utf-8")
    # Renderer sentences must be passed to t(). This deliberately targets visible text
    # properties, leaving API paths, data attributes, CSS selectors, and user content alone.
    for line_number, line in enumerate(source.splitlines(), 1):
        if re.search(r'\btext:\s*"[^"\\]*(?:[.!?…]|\s)[^"\\]*"', line):
            failures.append(f"app.js:{line_number}: visible renderer text must use t()")

    if failures:
        print("i18n check failed:", *failures, sep="\n- ", file=sys.stderr)
        return 1
    print(f"i18n check passed: {len(expected)} keys across {', '.join(LANGUAGES)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

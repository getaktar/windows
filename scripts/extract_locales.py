#!/usr/bin/env python3
"""Builds src/locales/<lang>.json from the macOS app's String Catalog.

The Windows app is translated from the same source strings as the Mac app,
so every translation made there carries over here. Keys stay the English
source text, with Swift's format specifiers (%@, %lld, %1$@...) rewritten
as numbered placeholders ({0}, {1}...) that both the frontend and the Rust
side fill in.

Strings that only exist on Windows (Credential Manager instead of Keychain,
"this PC" instead of "this Mac", tray wording...) live in
scripts/windows_strings.json with their translations, and win over a
catalog entry with the same key.

Usage: scripts/extract_locales.py [path/to/Localizable.xcstrings]
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CATALOG = ROOT.parent / "mac" / "Sources" / "Aktar" / "Localizable.xcstrings"
EXTRAS = ROOT / "scripts" / "windows_strings.json"
OUT = ROOT / "src" / "locales"
LANGUAGES = ["en", "tr", "de", "fr", "es", "pt-BR", "ja", "zh-Hans"]

SPECIFIER = re.compile(r"%(?:(\d+)\$)?(?:@|lld|ld|d)")


def convert(text: str) -> str:
    """Rewrites %@ / %lld / %1$@ as {0} / {1}, keeping positional order."""
    counter = 0

    def replace(match: re.Match) -> str:
        nonlocal counter
        if match.group(1):
            return "{%d}" % (int(match.group(1)) - 1)
        index = counter
        counter += 1
        return "{%d}" % index

    return SPECIFIER.sub(replace, text)


def main() -> None:
    catalog_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_CATALOG
    strings = json.loads(catalog_path.read_text(encoding="utf-8"))["strings"]
    extras = json.loads(EXTRAS.read_text(encoding="utf-8"))

    tables = {language: {} for language in LANGUAGES}
    for key, entry in strings.items():
        if not key or entry.get("shouldTranslate") is False:
            continue
        converted_key = convert(key)
        tables["en"][converted_key] = converted_key
        for language, localization in entry.get("localizations", {}).items():
            if language not in tables:
                continue
            value = localization.get("stringUnit", {}).get("value")
            if value:
                tables[language][converted_key] = convert(value)

    for key, translations in extras.items():
        if key.startswith("//"):
            continue
        tables["en"][key] = translations.get("en", key)
        for language in LANGUAGES[1:]:
            value = translations.get(language)
            if value is None:
                raise SystemExit(f"windows_strings.json: '{key}' has no {language} translation")
            tables[language][key] = value

    OUT.mkdir(parents=True, exist_ok=True)
    for language, table in tables.items():
        path = OUT / f"{language}.json"
        path.write_text(
            json.dumps(dict(sorted(table.items())), ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"{path.relative_to(ROOT)}: {len(table)} strings")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Builds the Windows icon sources from the shared artwork.

- src-tauri/icons/app-icon.png: the app icon (the white mark on the dark
  rounded tile, like the Mac icon). Feed it to `pnpm tauri icon` to get
  icon.ico and the PNG sizes the bundle uses.
- src-tauri/icons/tray-*-taskbar.png: monochrome notification area glyphs,
  dark for the light taskbar and white for the dark taskbar, matching the
  system's own tray icons.

Needs Pillow. Usage: scripts/make_icons.py [path/to/app-icon-artwork.png]
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ARTWORK = ROOT.parent / "mac" / "docs" / "app-icon-artwork.png"
ICONS = ROOT / "src-tauri" / "icons"

TILE_COLOR = (21, 21, 21, 255)
GLYPH_ON_LIGHT = (28, 28, 28)
GLYPH_ON_DARK = (255, 255, 255)


def glyph_mask(artwork: Image.Image) -> Image.Image:
    """The white mark as an alpha mask (the artwork is white on near-black)."""
    gray = artwork.convert("L")
    # Map the dark background to 0 and the white mark to 255.
    return gray.point(lambda value: max(0, min(255, int((value - 30) * 255 / 225))))


def app_icon(artwork: Image.Image, size: int = 1024) -> Image.Image:
    mask = glyph_mask(artwork).resize((size, size), Image.LANCZOS)
    icon = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    inset = round(size * 0.06)
    tile = Image.new("L", (size, size), 0)
    ImageDraw.Draw(tile).rounded_rectangle(
        (inset, inset, size - inset, size - inset), radius=round(size * 0.2), fill=255
    )
    icon.paste(Image.new("RGBA", (size, size), TILE_COLOR), (0, 0), tile)
    icon.paste(Image.new("RGBA", (size, size), (255, 255, 255, 255)), (0, 0), mask)
    return icon


def tray_icon(artwork: Image.Image, color: tuple, size: int = 32) -> Image.Image:
    mask = glyph_mask(artwork)
    box = mask.getbbox()
    mask = mask.crop(box)
    # Square it up, with a little breathing room like the system glyphs.
    side = round(max(mask.size) * 1.12)
    square = Image.new("L", (side, side), 0)
    square.paste(mask, ((side - mask.width) // 2, (side - mask.height) // 2))
    square = square.resize((size, size), Image.LANCZOS)
    solid = Image.new("RGBA", (size, size), color + (255,))
    solid.putalpha(square)
    return solid


def main() -> None:
    artwork_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_ARTWORK
    artwork = Image.open(artwork_path).convert("RGB")
    ICONS.mkdir(parents=True, exist_ok=True)
    app_icon(artwork).save(ICONS / "app-icon.png")
    tray_icon(artwork, GLYPH_ON_LIGHT).save(ICONS / "tray-light-taskbar.png")
    tray_icon(artwork, GLYPH_ON_DARK).save(ICONS / "tray-dark-taskbar.png")
    print("Wrote app-icon.png and tray icons to", ICONS.relative_to(ROOT))


if __name__ == "__main__":
    main()

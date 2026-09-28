#!/usr/bin/env python3
"""Builds the MSIX (Microsoft Store) logos from src-tauri/icons/app-icon.png.

Windows picks the variant that fits each place it shows the app: the
Start menu and taskbar (Square44x44Logo, by target size and display
scale), Start tiles (Square150x150Logo, Wide310x150Logo), and the Store
and installer (StoreLogo). "altform-unplated" variants are what the
taskbar uses; the icon's own dark tile already works there.

Needs Pillow. Usage: scripts/make_msix_assets.py
"""
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "src-tauri" / "icons" / "app-icon.png"
ASSETS = ROOT / "packaging" / "msix" / "Assets"

SCALES = [100, 125, 150, 200, 400]


def fitted(icon: Image.Image, width: int, height: int, fill: float) -> Image.Image:
    """The icon centered on a transparent canvas, `fill` of the shorter side."""
    side = round(min(width, height) * fill)
    canvas = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    resized = icon.resize((side, side), Image.LANCZOS)
    canvas.paste(resized, ((width - side) // 2, (height - side) // 2), resized)
    return canvas


def main() -> None:
    # Trimmed to the tile itself: the source has a margin for macOS.
    icon = Image.open(SOURCE).convert("RGBA")
    icon = icon.crop(icon.getbbox())
    ASSETS.mkdir(parents=True, exist_ok=True)
    for old in ASSETS.glob("*.png"):
        old.unlink()

    def save(image: Image.Image, name: str) -> None:
        image.save(ASSETS / name, optimize=True)

    for scale in SCALES:
        factor = scale / 100
        save(fitted(icon, round(44 * factor), round(44 * factor), 1.0), f"Square44x44Logo.scale-{scale}.png")
        save(fitted(icon, round(150 * factor), round(150 * factor), 0.6), f"Square150x150Logo.scale-{scale}.png")
        save(fitted(icon, round(310 * factor), round(150 * factor), 0.6), f"Wide310x150Logo.scale-{scale}.png")
        save(fitted(icon, round(50 * factor), round(50 * factor), 1.0), f"StoreLogo.scale-{scale}.png")
    for size in [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256]:
        image = fitted(icon, size, size, 1.0)
        save(image, f"Square44x44Logo.targetsize-{size}.png")
        save(image, f"Square44x44Logo.targetsize-{size}_altform-unplated.png")
        save(image, f"Square44x44Logo.targetsize-{size}_altform-lightunplated.png")
    print(f"Wrote {len(list(ASSETS.glob('*.png')))} logos to {ASSETS.relative_to(ROOT)}")


if __name__ == "__main__":
    main()

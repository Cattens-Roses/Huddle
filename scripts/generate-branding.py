"""Render the Bootstrap example SVG to desktop icons (requires CairoSVG and Pillow).

Source and MIT license are kept in assets/branding.
"""
from io import BytesIO
from pathlib import Path
import runpy

import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
SVG = (ROOT / "assets/branding/example.svg").read_text(encoding="utf-8")


def render(size):
    data = cairosvg.svg2png(
        bytestring=SVG.encode("utf-8"), output_width=size * 4, output_height=size * 4
    )
    return Image.open(BytesIO(data)).convert("RGBA").resize(
        (size, size), Image.Resampling.LANCZOS
    )


if __name__ == "__main__":
    render(256).save(ROOT / "assets/windows/huddle.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    render(1024).save(ROOT / "assets/macos/huddle.icns")
    (ROOT / "assets/linux/huddle.svg").write_text(SVG, encoding="utf-8")
    (ROOT / "assets/icons/common/huddle.svg").write_text(SVG, encoding="utf-8")
    for path in (ROOT / "assets/linux/icons/hicolor").glob("*/apps/huddle.png"):
        render(int(path.parents[1].name.split("x")[0])).save(path)
    render(64).save(ROOT / "assets/tray/huddle.png")
    runpy.run_path(str(ROOT / "scripts/generate-windows-wizard.py"), run_name="__main__")

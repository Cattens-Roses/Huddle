"""Generate Inno Setup panels from the same icon embedded in Huddle.exe.

Run after changing assets/windows/huddle.ico. Requires Pillow.
"""
from pathlib import Path

from PIL import Image

ASSETS = Path(__file__).resolve().parents[1] / "assets/windows"


def main():
    with Image.open(ASSETS / "huddle.ico") as source:
        icon = source.convert("RGBA")
    for name, size, icon_size in (
        ("wizard.bmp", (164, 314), 112),
        ("wizard-small.bmp", (55, 55), 44),
    ):
        panel = Image.new("RGB", size, "white")
        resized = icon.resize((icon_size, icon_size), Image.Resampling.LANCZOS)
        panel.paste(resized, ((size[0] - icon_size) // 2, (size[1] - icon_size) // 2), resized)
        panel.save(ASSETS / name)


if __name__ == "__main__":
    main()

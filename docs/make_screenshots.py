"""Builds the README's screenshots from Pollen's demo renders, which use
made-up data (Windows' own apps, a sample song, city and readings), on a
generated wallpaper. Nothing personal ends up in them.

    pollen.exe --demo-shots <folder>
    python docs/make_screenshots.py <folder>
"""

import math
import os
import random
import sys

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "screenshots")
W, H = 1920, 1080


def wallpaper():
    """Soft color fields on a deep navy, in the Midnight theme's colors."""
    random.seed(7)
    base = Image.new("RGB", (W, H))
    draw = ImageDraw.Draw(base)
    for y in range(H):
        t = y / (H - 1)
        draw.line([(0, y), (W, y)], fill=(int(14 + 8 * t), int(18 + 6 * t), int(40 + 10 * t)))
    blobs = Image.new("RGB", (W, H), (0, 0, 0))
    d = ImageDraw.Draw(blobs)
    for (cx, cy, r, color) in [
        (420, 300, 520, (70, 96, 230)),
        (1500, 260, 460, (122, 92, 255)),
        (1150, 820, 560, (255, 138, 92)),
        (300, 900, 380, (53, 160, 200)),
    ]:
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=color)
    blobs = blobs.filter(ImageFilter.GaussianBlur(220))
    out = Image.blend(base, blobs, 0.55)
    # A little grain so the gradients do not band.
    noise = Image.effect_noise((W, H), 6).convert("RGB")
    return Image.blend(out, noise, 0.03).convert("RGBA")


def load(folder, name):
    return Image.open(os.path.join(folder, name)).convert("RGBA")


def place_top(canvas, layer):
    canvas.alpha_composite(layer, ((W - layer.width) // 2, 0))


def place_bottom(canvas, layer):
    canvas.alpha_composite(layer, ((W - layer.width) // 2, H - layer.height))


def crop_around(image, box, margin):
    left, top, right, bottom = box
    return image.crop((max(0, left - margin), max(0, top - margin), min(W, right + margin), min(H, bottom + margin)))


def save(image, name, width=None):
    if width and image.width > width:
        image = image.resize((width, round(image.height * width / image.width)), Image.LANCZOS)
    path = os.path.join(OUT, name)
    image.convert("RGB").save(path, optimize=True)
    print("wrote", path)


def main():
    folder = sys.argv[1]
    os.makedirs(OUT, exist_ok=True)
    wall = wallpaper()
    dock = load(folder, "dock.png")

    # The whole desktop: the dock below, the notch open on Music above.
    desktop = wall.copy()
    place_bottom(desktop, dock)
    place_top(desktop, load(folder, "notch-music.png"))
    save(desktop, "desktop.png", 1600)

    # The dock alone.
    strip = wall.copy()
    place_bottom(strip, dock)
    bbox = dock.getbbox()
    top = H - dock.height + bbox[1]
    save(strip.crop(((W - 1100) // 2, top - 30, (W + 1100) // 2, H)), "dock.png")

    # Each notch state, cropped around it.
    for name in ["closed", "volume", "music", "control-center", "status", "weather", "calendar", "settings"]:
        layer = load(folder, f"notch-{name}.png")
        canvas = wall.copy()
        place_top(canvas, layer)
        x0 = (W - layer.width) // 2
        l, t, r, b = layer.getbbox()
        box = (x0 + l, 0, x0 + r, b)
        if name in ("closed", "volume"):
            shot = crop_around(canvas, (W // 2 - 200, 0, W // 2 + 200, b), 24)
        else:
            shot = crop_around(canvas, box, 28)
        save(shot, f"notch-{name}.png")


if __name__ == "__main__":
    main()

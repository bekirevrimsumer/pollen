"""Draws Pollen's app icon: a flower on a dark rounded tile, in the Midnight
theme's colors. Writes assets/pollen.ico (16-256 px), icon-256.png for the README and a preview PNG.

    python assets/make_icon.py
"""

import math
import os

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
S = 1024  # drawn large, then scaled down


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def vertical_gradient(size, top, bottom):
    img = Image.new("RGBA", (size, size))
    draw = ImageDraw.Draw(img)
    for y in range(size):
        draw.line([(0, y), (size, y)], fill=lerp(top, bottom, y / (size - 1)) + (255,))
    return img


def radial_gradient(size, inner, outer):
    img = Image.new("RGBA", (size, size))
    px = img.load()
    c = (size - 1) / 2
    for y in range(size):
        for x in range(size):
            t = min(1.0, math.hypot(x - c, y - c) / c)
            px[x, y] = lerp(inner, outer, t) + (255,)
    return img


def tile():
    """The rounded tile, a dark blue-black gradient with a soft rim."""
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([40, 40, S - 40, S - 40], radius=230, fill=255)
    bg = vertical_gradient(S, (24, 32, 54), (8, 11, 19))
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(bg, (0, 0), mask)
    rim = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(rim).rounded_rectangle([42, 42, S - 42, S - 42], radius=228, outline=(122, 162, 255, 70), width=6)
    return Image.alpha_composite(out, rim)


def petal_layer(angle_deg):
    """One petal: an ellipse pointing outward from the center."""
    w, h = 250, 360
    petal = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    grad = vertical_gradient(max(w, h), (176, 200, 255), (94, 139, 255)).resize((w, h))
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).ellipse([0, 0, w - 1, h - 1], fill=255)
    petal.paste(grad, (0, 0), mask)
    rotated = petal.rotate(-angle_deg, resample=Image.BICUBIC, expand=True)
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    # Petal center sits out from the flower's center along the angle.
    a = math.radians(angle_deg - 90)
    cx, cy = S / 2 + 205 * math.cos(a), S / 2 + 205 * math.sin(a)
    layer.paste(rotated, (round(cx - rotated.width / 2), round(cy - rotated.height / 2)), rotated)
    return layer


def flower():
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    for i in range(6):
        layer = Image.alpha_composite(layer, petal_layer(i * 60))
    # A soft shadow under the center lifts it off the petals.
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).ellipse([S / 2 - 150, S / 2 - 140, S / 2 + 150, S / 2 + 160], fill=(10, 14, 30, 140))
    layer = Image.alpha_composite(layer, shadow.filter(ImageFilter.GaussianBlur(28)))
    # The pollen: a warm disc with a few grains.
    r = 140
    disc = radial_gradient(2 * r, (255, 230, 150), (255, 176, 64))
    mask = Image.new("L", (2 * r, 2 * r), 0)
    ImageDraw.Draw(mask).ellipse([0, 0, 2 * r - 1, 2 * r - 1], fill=255)
    layer.paste(disc, (S // 2 - r, S // 2 - r), mask)
    grains = ImageDraw.Draw(layer)
    for gx, gy, gr in [(-50, -40, 18), (35, -55, 15), (55, 20, 17), (-25, 45, 16), (-65, 15, 12), (10, -5, 13)]:
        grains.ellipse([S / 2 + gx - gr, S / 2 + gy - gr, S / 2 + gx + gr, S / 2 + gy + gr], fill=(214, 128, 30, 200))
    return layer


def icon():
    art = Image.alpha_composite(tile(), flower())
    return art


def main():
    art = icon()
    sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
    frames = [art.resize((n, n), Image.LANCZOS) for n in sizes]
    frames[-1].save(os.path.join(HERE, "pollen.ico"), sizes=[(n, n) for n in sizes], append_images=frames[:-1])
    frames[-1].save(os.path.join(HERE, "icon-256.png"))
    preview = Image.new("RGBA", (sum(sizes[:6]) + 256 + 20 * 7, 256), (40, 44, 52, 255))
    x = 10
    for n, f in zip(sizes, frames):
        if n in (16, 24, 32, 48, 64, 128, 256):
            preview.alpha_composite(f, (x, (256 - n) // 2))
            x += n + 20
    preview.save(os.path.join(HERE, "icon-preview.png"))
    print("wrote", os.path.join(HERE, "pollen.ico"))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Generate the rust-ime application / tray icon set.

A rounded-square gradient tile with a white "R" and three candidate dots —
readable down to 16 px.

    python3 scripts/gen-icons.py
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "app" / "icons"
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"

# Gradient endpoints (#4F8CFF → #7B5CFF).
C1 = (0x4F, 0x8C, 0xFF)
C2 = (0x7B, 0x5C, 0xFF)

SS = 4  # supersampling factor


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def make(size: int) -> Image.Image:
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))

    # Vertical gradient.
    grad = Image.new("RGBA", (s, s))
    gd = ImageDraw.Draw(grad)
    for y in range(s):
        gd.line([(0, y), (s, y)], fill=(*lerp(C1, C2, y / max(1, s - 1)), 255))

    # Rounded-rect mask.
    mask = Image.new("L", (s, s), 0)
    md = ImageDraw.Draw(mask)
    radius = int(s * 0.235)
    md.rounded_rectangle([0, 0, s - 1, s - 1], radius=radius, fill=255)
    img.paste(grad, (0, 0), mask)

    # Subtle top highlight.
    hl = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    hd = ImageDraw.Draw(hl)
    hd.rounded_rectangle(
        [int(s * 0.06), int(s * 0.05), int(s * 0.94), int(s * 0.45)],
        radius=int(s * 0.20),
        fill=(255, 255, 255, 22),
    )
    img = Image.alpha_composite(img, hl)

    # White "R".
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype(FONT, int(s * 0.58))
    box = d.textbbox((0, 0), "R", font=font)
    tw, th = box[2] - box[0], box[3] - box[1]
    tx = (s - tw) / 2 - box[0]
    ty = (s - th) / 2 - box[1] - int(s * 0.045)
    d.text((tx, ty), "R", font=font, fill=(255, 255, 255, 255))

    # Three candidate dots under the letter.
    r = int(s * 0.035)
    gap = int(s * 0.105)
    cy = int(s * 0.80)
    cx = s // 2
    for i, alpha in enumerate((255, 170, 110)):
        x = cx + (i - 1) * gap
        d.ellipse([x - r, cy - r, x + r, cy + r], fill=(255, 255, 255, alpha))

    return img.resize((size, size), Image.LANCZOS)


def main():
    ICONS.mkdir(parents=True, exist_ok=True)
    # Also expose a copy for the settings UI (served from dist/).
    dist = ROOT / "dist"
    dist.mkdir(parents=True, exist_ok=True)

    for size, name in [
        (512, "512x512.png"),
        (256, "256x256.png"),
        (256, "128x128@2x.png"),
        (128, "128x128.png"),
        (64, "64x64.png"),
        (32, "32x32.png"),
        (48, "icon.png"),
    ]:
        img = make(size)
        img.save(ICONS / name)
        print("wrote", ICONS / name)
        if name == "256x256.png":
            img.save(dist / "logo.png")


if __name__ == "__main__":
    main()

"""Draws assets/lumen.ico: the amber logo tile with a waveform, as in the
title bar. Run: python tools/make_icon.py (needs Pillow)."""
from PIL import Image, ImageDraw

ACCENT = (0xF2, 0xB5, 0x44, 255)
INK = (0x0B, 0x0C, 0x0F, 255)
# Bar heights (fraction of the tile), like Lucide's audio-waveform.
BARS = [0.18, 0.42, 0.66, 0.42, 0.78, 0.5, 0.26]


def tile(size: int) -> Image.Image:
    s = size * 4  # draw large, then downsample for smooth edges
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((0, 0, s - 1, s - 1), radius=int(s * 0.24), fill=ACCENT)
    n = len(BARS)
    span = s * 0.62
    w = span / (n * 1.7)
    x0 = (s - span) / 2
    step = (span - w) / (n - 1)
    for i, h in enumerate(BARS):
        x = x0 + i * step
        hh = s * h
        d.rounded_rectangle(
            (x, (s - hh) / 2, x + w, (s + hh) / 2), radius=w / 2, fill=INK
        )
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    big = tile(256)
    big.save("assets/lumen.ico", sizes=[(s, s) for s in sizes])
    big.save("assets/lumen-256.png")
    print("wrote assets/lumen.ico")

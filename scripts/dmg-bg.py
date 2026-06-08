#!/usr/bin/env python3
"""Generate a clean install-window background for the Oxide dmg, matching the
electron-builder default proportions (540x380) used by apps like synara."""
import sys
from PIL import Image, ImageDraw, ImageFont

W, H = 540, 380
OUT = sys.argv[1] if len(sys.argv) > 1 else "dist/dmg-bg.png"

img = Image.new("RGB", (W, H), (14, 14, 14))  # #0e0e0e
d = ImageDraw.Draw(img)

def font(sz):
    for p in ("/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc", "/Library/Fonts/Arial.ttf"):
        try:
            return ImageFont.truetype(p, sz)
        except Exception:
            continue
    return ImageFont.load_default()

def center(text, y, f, fill):
    w = d.textlength(text, font=f)
    d.text(((W - w) / 2, y), text, font=f, fill=fill)

center("Install Oxide", 40, font(20), (240, 240, 240))
center("Drag the app onto the Applications folder", 70, font(12), (140, 140, 140))

# subtle arrow between the two icon slots (icons centered at y~205)
ay = 205
d.line([(238, ay), (300, ay)], fill=(90, 90, 96), width=2)
d.polygon([(308, ay), (296, ay - 7), (296, ay + 7)], fill=(90, 90, 96))

img.save(OUT)
print("wrote", OUT)

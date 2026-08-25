from math import hypot
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter


ROOT = Path(__file__).resolve().parents[1] / "icons"
SIZE = 1024


def blend(a, b, amount):
    return tuple(round(a[index] * (1 - amount) + b[index] * amount) for index in range(3))


def make_icon(size):
    image = Image.new("RGB", (size, size), "white")
    pixels = image.load()
    center = size / 2
    outer = size * 0.36
    ring = size * 0.33
    core = size * 0.30

    for y in range(size):
        for x in range(size):
            distance = hypot(x - center, y - center)
            if distance <= outer:
                pixels[x, y] = (16, 20, 28)
            if distance <= ring:
                angle = ((x - center) - (y - center)) / (size * 0.64) + 0.5
                pixels[x, y] = blend((33, 107, 255), (255, 101, 184), max(0, min(1, angle)))
            if distance <= core:
                amount = max(0, min(1, distance / core))
                pixels[x, y] = blend((218, 250, 255), (55, 103, 248), amount)

    glow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    glow_draw = ImageDraw.Draw(glow)
    glow_draw.ellipse((size * 0.21, size * 0.17, size * 0.62, size * 0.59), fill=(150, 248, 255, 110))
    glow_draw.ellipse((size * 0.38, size * 0.40, size * 0.84, size * 0.82), fill=(255, 120, 205, 100))
    image = Image.alpha_composite(image.convert("RGBA"), glow.filter(ImageFilter.GaussianBlur(size * 0.05)))

    draw = ImageDraw.Draw(image)
    dot = size * 0.07
    draw.ellipse((center - dot, center - dot, center + dot, center + dot), fill=(255, 255, 255, 238))
    line_color = (16, 20, 28, 255)
    line_width = max(8, round(size * 0.034))
    bars = [(-0.14, 0.11), (-0.05, 0.19), (0.05, 0.15), (0.14, 0.09)]
    for offset, half_height in bars:
        x = center + offset * size
        draw.line((x, center - half_height * size, x, center + half_height * size), fill=line_color, width=line_width)
    sparkle = size * 0.034
    draw.ellipse((size * 0.65 - sparkle, size * 0.32 - sparkle, size * 0.65 + sparkle, size * 0.32 + sparkle), fill=(255, 255, 255, 230))
    return image


def save_icon():
    source = make_icon(SIZE)
    sizes = {
        "icon.png": 512,
        "32x32.png": 32,
        "64x64.png": 64,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "Square30x30Logo.png": 30,
        "Square44x44Logo.png": 44,
        "Square71x71Logo.png": 71,
        "Square89x89Logo.png": 89,
        "Square107x107Logo.png": 107,
        "Square142x142Logo.png": 142,
        "Square150x150Logo.png": 150,
        "Square284x284Logo.png": 284,
        "Square310x310Logo.png": 310,
        "StoreLogo.png": 50,
    }
    for name, size in sizes.items():
        source.resize((size, size), Image.Resampling.LANCZOS).convert("RGBA").save(ROOT / name)
    source.resize((256, 256), Image.Resampling.LANCZOS).save(ROOT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])


if __name__ == "__main__":
    save_icon()

#!/usr/bin/env python3
"""Generate the site favicon from a single character.

The site mark is one Chinese character drawn as an outlined path rather than as
`<text>`. That matters: an SVG favicon that relies on a font renders differently
— or not at all — on a machine without that font, and the whole point of the
icon is that it always looks the same.

Run with a Python that has `fonttools` (a venv is enough):

    python3 -m venv /tmp/iconvenv && /tmp/iconvenv/bin/pip install fonttools
    /tmp/iconvenv/bin/python scripts/make-icon.py

Outputs into static/: favicon.svg, favicon.ico, favicon-32.png,
apple-touch-icon.png.
"""
import pathlib
import sys

from fontTools.misc.transform import Transform
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

# The mark. 技 reads clearly even at 16px, where more complex characters (博,
# for instance) turn to mud.
GLYPH = "技"

# Matches --accent in static/style.css.
BLUE = "#2f6feb"
WHITE = "#ffffff"

# 32 is the SVG coordinate space; the glyph is sized to leave a margin inside
# the rounded square rather than touching the edges.
VIEWBOX = 32
GLYPH_BOX = 19.0
CORNER_RADIUS = 7

FONT_CANDIDATES = [
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 1),
    ("/System/Library/Fonts/STHeiti Medium.ttc", 0),
    ("/System/Library/Fonts/PingFang.ttc", 4),
]

ROOT = pathlib.Path(__file__).resolve().parent.parent
STATIC = ROOT / "static"


def find_font():
    for path, index in FONT_CANDIDATES:
        if pathlib.Path(path).exists():
            return path, index
    raise SystemExit("no CJK font found; edit FONT_CANDIDATES")


def glyph_path(font_path, index):
    """The character as an SVG path, centred in the viewBox."""
    font = TTFont(font_path, fontNumber=index)
    glyphset = font.getGlyphSet()
    name = font.getBestCmap()[ord(GLYPH)]
    glyph = glyphset[name]

    bounds = BoundsPen(glyphset)
    glyph.draw(bounds)
    x0, y0, x1, y1 = bounds.bounds

    scale = GLYPH_BOX / max(x1 - x0, y1 - y0)

    # Font coordinates put y up and the origin on the baseline; SVG puts y down.
    # Flipping the scale and recentring on the box reconciles the two.
    transform = Transform(
        scale,
        0,
        0,
        -scale,
        VIEWBOX / 2 - (x0 + x1) / 2 * scale,
        VIEWBOX / 2 + (y0 + y1) / 2 * scale,
    )

    pen = SVGPathPen(glyphset)
    glyph.draw(TransformPen(pen, transform))
    return pen.getCommands()


def write_svg(path_data, out):
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {VIEWBOX} {VIEWBOX}" '
        f'role="img" aria-label="{GLYPH}">\n'
        f'  <rect width="{VIEWBOX}" height="{VIEWBOX}" rx="{CORNER_RADIUS}" fill="{BLUE}"/>\n'
        f'  <path d="{path_data}" fill="{WHITE}"/>\n'
        f"</svg>\n"
    )
    out.write_text(svg)
    return len(svg.encode())


def draw_icon(font_path, index, size):
    """One rasterised icon at the given size."""
    # The rounded square is drawn oversized and downsampled, because Pillow
    # has no antialiased rounded rectangle.
    supersample = 4
    big = Image.new("RGBA", (size * supersample,) * 2, (0, 0, 0, 0))
    ImageDraw.Draw(big).rounded_rectangle(
        [0, 0, size * supersample - 1, size * supersample - 1],
        radius=CORNER_RADIUS / VIEWBOX * size * supersample,
        fill=BLUE,
    )
    image = big.resize((size, size), Image.LANCZOS)

    draw = ImageDraw.Draw(image)
    # Match GLYPH_BOX / VIEWBOX from the SVG so both agree.
    font = ImageFont.truetype(font_path, int(size * GLYPH_BOX / VIEWBOX), index=index)
    box = draw.textbbox((0, 0), GLYPH, font=font)
    width, height = box[2] - box[0], box[3] - box[1]
    draw.text(
        ((size - width) / 2 - box[0], (size - height) / 2 - box[1]),
        GLYPH,
        font=font,
        fill=WHITE,
    )

    return image


def main():
    font_path, index = find_font()
    print(f"font: {font_path} (face {index})")
    print(f"glyph: {GLYPH}")

    STATIC.mkdir(exist_ok=True)

    path_data = glyph_path(font_path, index)
    size = write_svg(path_data, STATIC / "favicon.svg")
    print(f"  favicon.svg            {size} B, path {len(path_data)} chars")

    for name, px in [("favicon-32.png", 32), ("apple-touch-icon.png", 180)]:
        draw_icon(font_path, index, px).save(STATIC / name)
        print(f"  {name:<22} {px}x{px}  {(STATIC / name).stat().st_size} B")

    # An .ico carrying 16/32/48 covers browsers that still ask for one. It has
    # to be built from a master larger than every size in it: Pillow downscales
    # but will not invent a 48px frame from a 32px source.
    ico_sizes = [(16, 16), (32, 32), (48, 48)]
    draw_icon(font_path, index, 256).save(STATIC / "favicon.ico", sizes=ico_sizes)
    print(f"  favicon.ico            {ico_sizes}  {(STATIC / 'favicon.ico').stat().st_size} B")

    return 0


if __name__ == "__main__":
    sys.exit(main())

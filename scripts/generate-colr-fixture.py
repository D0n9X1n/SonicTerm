#!/usr/bin/env python3
"""Generate the COLRv1 paint fixture font for the FreeType COLR rasterizer tests.

The fixture is `crates/sonicterm-font/test-fonts/colr-paint-fixture.ttf`. Each
colour glyph isolates one conversion of the FreeType COLRv1 walker, so a pixel
test can tell a correct mapping from a mistaken one:

- `lin` (U+E000): a 1600-unit square filled with a vertical red-to-blue linear
  gradient, red at the bottom; gradient anchors in font units.
- `rad` (U+E001): the square filled with a radial gradient, centre (800, 800),
  radii 200 and 800, red to blue.
- `sweep` (U+E002): the square filled with a full-turn sweep gradient in four
  hard bands (red, green, blue, yellow) around its centre.
- `scaled` (U+E003): an origin marker and the square scaled by 0.5 around
  (400, 1200), a pivot whose x and y differ.
- `rotated` (U+E004): an origin marker and the square rotated 180 degrees
  around (400, 1200).
- `skewed` (U+E005): an origin marker and the square skewed 30 degrees in x
  around (0, 800).
- `moved` (U+E006): an origin marker and a 400-unit square under an affine
  transform translating it by (800, 200).
- `clipped` (U+E007): the red square under a ClipBox of (0, 0, 800, 1600).

Every glyph has a ClipBox. The units per em are 2048, so no test size maps the
font onto a whole number of pixels. Run it with fontTools installed:

    python3 scripts/generate-colr-fixture.py

The output is deterministic: the head table's timestamps are fixed.
"""

from __future__ import annotations

from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUTPUT = Path(__file__).resolve().parent.parent / "crates/sonicterm-font/test-fonts/colr-paint-fixture.ttf"
UNITS_PER_EM = 2048
# 2024-01-01T00:00:00Z in seconds since 1904-01-01, the head table's epoch.
FIXED_TIMESTAMP = 3786825600
RED, BLUE, GREEN, YELLOW = 0, 1, 2, 3
PALETTE = [(1.0, 0.0, 0.0, 1.0), (0.0, 0.0, 1.0, 1.0), (0.0, 1.0, 0.0, 1.0), (1.0, 1.0, 0.0, 1.0)]
OUTLINE_GLYPHS = {"square": 1600, "marker": 100, "small": 400}
COLOR_GLYPHS = ["lin", "rad", "sweep", "scaled", "rotated", "skewed", "moved", "clipped"]
FIRST_CODEPOINT = 0xE000


def square_outline(side_units: int):
    """A closed square contour from the origin to (`side_units`, `side_units`)."""
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((0, side_units))
    pen.lineTo((side_units, side_units))
    pen.lineTo((side_units, 0))
    pen.closePath()
    return pen.glyph()


def empty_outline():
    """A glyph with no contours, used for .notdef and every colour base glyph."""
    return TTGlyphPen(None).glyph()


def color_line(stops):
    """A pad-extended colour line from `(offset, palette_index)` pairs."""
    return {
        "Extend": "pad",
        "ColorStop": [
            {"StopOffset": offset, "PaletteIndex": index, "Alpha": 1.0} for offset, index in stops
        ],
    }


def solid_glyph(glyph_name: str, palette_index: int):
    """PaintGlyph of `glyph_name` filled with one palette colour."""
    return {
        "Format": 10,
        "Glyph": glyph_name,
        "Paint": {"Format": 2, "PaletteIndex": palette_index, "Alpha": 1.0},
    }


def with_marker(paint):
    """Layers: a green origin marker under `paint`, so the ink always starts at x = 0."""
    return {"Format": 1, "Layers": [solid_glyph("marker", GREEN), paint]}


def color_paints():
    """The paint graph of each colour glyph, keyed by glyph name."""
    red_square = solid_glyph("square", RED)
    return {
        "lin": {
            "Format": 10,
            "Glyph": "square",
            "Paint": {
                "Format": 4,
                "ColorLine": color_line([(0.0, RED), (1.0, BLUE)]),
                "x0": 0,
                "y0": 0,
                "x1": 0,
                "y1": 1600,
                "x2": 1600,
                "y2": 0,
            },
        },
        "rad": {
            "Format": 10,
            "Glyph": "square",
            "Paint": {
                "Format": 6,
                "ColorLine": color_line([(0.0, RED), (1.0, BLUE)]),
                "x0": 800,
                "y0": 800,
                "r0": 200,
                "x1": 800,
                "y1": 800,
                "r1": 800,
            },
        },
        "sweep": {
            "Format": 10,
            "Glyph": "square",
            "Paint": {
                "Format": 8,
                "ColorLine": color_line(
                    [
                        (0.0, RED),
                        (0.25, RED),
                        (0.25, GREEN),
                        (0.5, GREEN),
                        (0.5, BLUE),
                        (0.75, BLUE),
                        (0.75, YELLOW),
                        (1.0, YELLOW),
                    ]
                ),
                "centerX": 800,
                "centerY": 800,
                "startAngle": 0.0,
                "endAngle": 360.0,
            },
        },
        "scaled": with_marker(
            {
                "Format": 18,
                "Paint": red_square,
                "scaleX": 0.5,
                "scaleY": 0.5,
                "centerX": 400,
                "centerY": 1200,
            }
        ),
        "rotated": with_marker(
            {"Format": 26, "Paint": red_square, "angle": 180.0, "centerX": 400, "centerY": 1200}
        ),
        "skewed": with_marker(
            {
                "Format": 30,
                "Paint": red_square,
                "xSkewAngle": 30.0,
                "ySkewAngle": 0.0,
                "centerX": 0,
                "centerY": 800,
            }
        ),
        "moved": with_marker(
            {
                "Format": 12,
                "Paint": solid_glyph("small", RED),
                "Transform": {"xx": 1.0, "yx": 0.0, "xy": 0.0, "yy": 1.0, "dx": 800, "dy": 200},
            }
        ),
        "clipped": red_square,
    }


CLIP_BOXES = {
    "lin": (0, 0, 1600, 1600),
    "rad": (0, 0, 1600, 1600),
    "sweep": (0, 0, 1600, 1600),
    "scaled": (0, 0, 1700, 1700),
    "rotated": (-1000, 0, 1700, 2500),
    "skewed": (-1200, 0, 2800, 1700),
    "moved": (0, 0, 1700, 1700),
    "clipped": (0, 0, 800, 1600),
}


def build() -> None:
    """Build the fixture and write it to OUTPUT."""
    glyph_order = [".notdef", *OUTLINE_GLYPHS, *COLOR_GLYPHS]
    builder = FontBuilder(UNITS_PER_EM, isTTF=True)
    builder.setupGlyphOrder(glyph_order)
    builder.setupCharacterMap(
        {FIRST_CODEPOINT + offset: name for offset, name in enumerate(COLOR_GLYPHS)}
    )
    outlines = {".notdef": empty_outline()}
    outlines.update({name: square_outline(side) for name, side in OUTLINE_GLYPHS.items()})
    outlines.update({name: empty_outline() for name in COLOR_GLYPHS})
    builder.setupGlyf(outlines)
    builder.setupHorizontalMetrics({name: (UNITS_PER_EM, 0) for name in glyph_order})
    builder.setupHorizontalHeader(ascent=1800, descent=-400)
    builder.setupNameTable({"familyName": "SonicTerm COLR Fixture", "styleName": "Regular"})
    builder.setupOS2(sTypoAscender=1800, sTypoDescender=-400, usWinAscent=1800, usWinDescent=400)
    builder.setupPost()
    builder.setupCPAL([PALETTE])
    builder.setupCOLR(color_paints(), version=1, clipBoxes=CLIP_BOXES)
    builder.font["head"].created = FIXED_TIMESTAMP
    builder.font["head"].modified = FIXED_TIMESTAMP
    # fontTools would stamp `modified` with the current time on save; turning that off keeps it fixed.
    builder.font.recalcTimestamp = False
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    builder.font.save(str(OUTPUT))
    print(f"wrote {OUTPUT}")


if __name__ == "__main__":
    build()

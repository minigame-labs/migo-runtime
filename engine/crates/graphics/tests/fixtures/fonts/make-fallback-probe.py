"""Build fallback-probe.ttf: one glyph, a solid em square at U+5F00, in a family no text names.

A test of the system font fallback needs a face the system can offer for a character the bundled
font lacks, and it needs to know exactly what that face draws. This is that face, and nothing else:
the square fills its em box (advance 1000/1000, from the baseline up to the ascender), so a glyph
drawn from it is told apart from the bundled font's hollow .notdef by its width and its ink.

  python3 make-fallback-probe.py      (needs fontTools; the output is committed, so tests do not)
"""

import pathlib

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen


def square(top: int) -> object:
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((0, top))
    pen.lineTo((1000, top))
    pen.lineTo((1000, 0))
    pen.closePath()
    return pen.glyph()


builder = FontBuilder(1000, isTTF=True)
builder.setupGlyphOrder([".notdef", "uni5F00"])
builder.setupCharacterMap({0x5F00: "uni5F00"})
builder.setupGlyf({".notdef": TTGlyphPen(None).glyph(), "uni5F00": square(800)})
builder.setupHorizontalMetrics({".notdef": (500, 0), "uni5F00": (1000, 0)})
builder.setupHorizontalHeader(ascent=800, descent=-200)
builder.setupNameTable({"familyName": "Migo Fallback Probe", "styleName": "Regular"})
builder.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200,
                 ulCodePageRange1=(1 << 18))  # the simplified-Chinese code page, so fontconfig lists zh
builder.setupPost()
builder.save(pathlib.Path(__file__).with_name("fallback-probe.ttf"))

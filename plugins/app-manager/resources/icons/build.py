"""Builds the system icon font from svg/*.svg.

Every icon becomes one glyph per color layer, in the Private Use Area at
the codepoints codepoints.json assigns it: four in a row, one per layer, so
views can name an icon's glyphs and they stay put when icons are added. A new
icon gets the next free block, appended to codepoints.json. A layer is the icon's shapes of one color, with the shapes painted
over them later cut out, so stacking the layers in order and tinting each one
reproduces the drawing. Shapes painted in the icon tile's color or the screen's
background are holes, not layers. Strokes are outlined first.

Writes:
  barracuda-icons.otf   the font the view builds bake from
  icons.json            each icon's layers: codepoint and color
  ../../crates/uikit/src/icons.rs    the same table for Rust

Needs fontTools, picosvg, and skia-pathops:

  python3 plugins/app-manager/resources/icons/build.py
"""

import json
import pathlib

import pathops
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.recordingPen import RecordingPen
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.transformPen import TransformPen
from fontTools.svgLib.path import parse_path
from picosvg.svg import SVG

HERE = pathlib.Path(__file__).resolve().parent
FIRST = 0xE000
UPM = 1000
# The em box the CJK faces use: icons sit where an ideograph would.
ASCENT, DESCENT = 880, -120
# Painted in these, a shape is a hole: the icon tile and the screen.
HOLES = {"#232327", "#09090B", "#27272A"}
# Layers are ordered so that the accent sits over the base.
ORDER = ["#52525B", "#F4F4F5", "#FAFAFA", "#CCFF00", "#ED8E04", "#FF7878"]
NAMES = {
    "#52525B": "dim",
    "#F4F4F5": "base",
    "#FAFAFA": "base",
    "#CCFF00": "accent",
    "#ED8E04": "warning",
    "#FF7878": "danger",
}


def layers(svg_text):
    """The icon's color layers in paint order, holes removed."""
    # Line icons are drawn in one color and tinted where they are shown.
    svg_text = svg_text.replace("currentColor", "#FAFAFA")
    pico = SVG.fromstring(svg_text).topicosvg()
    painted = {}
    order = []
    for shape in pico.shapes():
        fill = (shape.fill or "").upper()
        if not fill.startswith("#") or fill == "NONE":
            continue
        path = pathops.Path()
        pen = path.getPen()
        parse_path(shape.d, pen)
        for color, existing in painted.items():
            painted[color] = pathops.op(existing, path, pathops.PathOp.DIFFERENCE)
        if fill in HOLES:
            continue
        if fill in painted:
            painted[fill] = pathops.op(painted[fill], path, pathops.PathOp.UNION)
        else:
            painted[fill] = path
            order.append(fill)
    order.sort(key=lambda color: ORDER.index(color) if color in ORDER else len(ORDER))
    return [(color, painted[color]) for color in order], pico.view_box()


def main():
    icons = {}
    glyphs = {}
    cmap = {}
    assigned = json.loads((HERE / "codepoints.json").read_text())
    for file in sorted((HERE / "svg").glob("*.svg")):
        if file.stem not in assigned:
            used = [int(value[2:], 16) for value in assigned.values()]
            assigned[file.stem] = f"U+{max(used, default=FIRST - 4) + 4:04X}"
    (HERE / "codepoints.json").write_text(json.dumps(assigned, indent=2) + "\n")
    for file in sorted((HERE / "svg").glob("*.svg")):
        name = file.stem
        codepoint = int(assigned[name][2:], 16)
        parts, box = layers(file.read_text())
        if len(parts) > 4:
            raise SystemExit(f"{name}: more than four color layers")
        scale = (ASCENT - DESCENT) / max(box.w, box.h)
        # Center the drawing in the em square, y up.
        dx = (UPM - box.w * scale) / 2 - box.x * scale
        dy = ASCENT - (ASCENT - DESCENT - box.h * scale) / 2 + box.y * scale
        entry = []
        for color, path in parts:
            glyph = f"uni{codepoint:04X}"
            pen = T2CharStringPen(UPM, None)
            recording = RecordingPen()
            path.draw(recording)
            recording.replay(TransformPen(pen, (scale, 0, 0, -scale, dx, dy)))
            glyphs[glyph] = pen.getCharString()
            cmap[codepoint] = glyph
            entry.append({"codepoint": codepoint, "color": color, "layer": NAMES.get(color, "base")})
            codepoint += 1
        icons[name] = entry

    order = [".notdef"] + list(glyphs)
    notdef = T2CharStringPen(UPM, None).getCharString()
    builder = FontBuilder(UPM, isTTF=False)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap(cmap)
    builder.setupCFF("BarracudaIcons", {"FullName": "Barracuda Icons"}, {".notdef": notdef, **glyphs}, {})
    builder.setupHorizontalMetrics({glyph: (UPM, 0) for glyph in order})
    builder.setupHorizontalHeader(ascent=ASCENT, descent=DESCENT)
    builder.setupNameTable({"familyName": "Barracuda Icons", "styleName": "Regular"})
    builder.setupOS2(sTypoAscender=ASCENT, sTypoDescender=DESCENT, usWinAscent=ASCENT, usWinDescent=-DESCENT)
    builder.setupPost()
    builder.save(HERE / "barracuda-icons.otf")

    (HERE / "icons.json").write_text(json.dumps(icons, indent=2) + "\n")

    rust = [
        "//! The system icon set: each icon's glyphs in the system icon font, one",
        "//! per color layer, generated by `resources/icons/build.py`. Show an icon",
        "//! by stacking its layers' text, tinted with each layer's color.",
        "",
        "/// One color layer of an icon.",
        "#[derive(Clone, Copy, PartialEq, Eq, Debug)]",
        "pub struct IconLayer {",
        "    /// The layer's glyph, a one-character string.",
        "    pub glyph: &'static str,",
        "    /// The layer's color as `#RRGGBB`.",
        "    pub color: &'static str,",
        "}",
        "",
        "/// The icon called `name`, as its layers from the bottom up.",
        "#[must_use]",
        "pub fn icon(name: &str) -> Option<&'static [IconLayer]> {",
        "    ICONS",
        "        .iter()",
        "        .find_map(|(icon, layers)| (*icon == name).then_some(*layers))",
        "}",
        "",
        "/// Every icon by name.",
        "pub const ICONS: &[(&str, &[IconLayer])] = &[",
    ]
    for name, entry in icons.items():
        layers_rs = ", ".join(
            f'IconLayer {{ glyph: "\\u{{{part["codepoint"]:04X}}}", color: "{part["color"]}" }}'
            for part in entry
        )
        rust.append(f'    ("{name}", &[{layers_rs}]),')
    rust.append("];")
    (HERE.parent.parent / "crates/uikit/src/icons.rs").write_text("\n".join(rust) + "\n")
    print(f"{len(icons)} icons, {len(glyphs)} glyphs")


if __name__ == "__main__":
    main()

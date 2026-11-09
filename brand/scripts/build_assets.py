"""Build Lemma's vector identity and campaign artwork, with outlined type.

Requires fonttools and brotli. All artwork is generated from local fonts and
geometry; it has no external image or font references.
"""
import hashlib
import html
import json
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parents[1]
C = json.loads((ROOT / "tokens.json").read_text())["color"]
FONTS = {}
for key, filename, weight in [
    ("serif", "InstrumentSerif-Regular.ttf", None),
    ("italic", "InstrumentSerif-Italic.ttf", None),
    ("sans", "Geist-Variable.ttf", 450),
    ("medium", "Geist-Variable.ttf", 550),
    ("mono", "GeistMono-Variable.ttf", 400),
]:
    font = TTFont(ROOT / "fonts" / filename)
    if weight is not None:
        font = instantiateVariableFont(font, {"wght": weight}, inplace=False)
    FONTS[key] = font


def label(value, x, y, size=16, face="sans", fill=None, spacing=0):
    """Outline a single line using the font's own glyph advances."""
    font = FONTS[face]
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = size / font["head"].unitsPerEm
    parts = []
    for char in value:
        name = cmap.get(ord(char), ".notdef")
        pen = SVGPathPen(glyphs)
        glyphs[name].draw(pen)
        parts.append(f'<path transform="translate({x:.3f} {y}) scale({scale:.6f} {-scale:.6f})" d="{pen.getCommands()}"/>')
        x += glyphs[name].width * scale + spacing
    return f'<g fill="{fill or C["ink"]}">{"".join(parts)}</g>'


def rect(x, y, w, h, fill, rx=0, stroke=None):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" fill="{fill}"' + (f' stroke="{stroke}"' if stroke else "") + '/>'


def mark(x=0, y=0, size=40, color=None):
    return f'<g transform="translate({x} {y}) scale({size / 32})" fill="{color or C["vermilion"]}"><path d="M2 2H10V22H30V30H2Z"/><path d="M18 2H30V14H22V10H18Z"/></g>'


def lockup(x=0, y=0, size=40, color=None, accent=None):
    return mark(x, y, size, accent or color) + label("lemma", x + size * 1.35, y + size * .83, size * .95, "medium", color)


def svg(width, height, title, body, desc=""):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc"><title id="title">{html.escape(title)}</title><desc id="desc">{html.escape(desc or title)}</desc>{body}</svg>\n'


def save(folder, name, width, height, title, body, desc=""):
    (ROOT / folder / name).write_text(svg(width, height, title, body, desc))


def line(x1, y1, x2, y2, color=None, width=1):
    return f'<path d="M{x1} {y1}H{x2}" fill="none" stroke="{color or C["line"]}" stroke-width="{width}"/>' if y1 == y2 else f'<path d="M{x1} {y1}L{x2} {y2}" fill="none" stroke="{color or C["line"]}" stroke-width="{width}"/>'


def diagram(x, y, scale=1, dark=False):
    """A conceptual, orthogonal relationship drawing; never a product screenshot."""
    fg = C["paper"] if dark else C["ink"]
    secondary = C["nightMuted"] if dark else C["muted"]
    border = C["nightLine"] if dark else C["line"]
    surface = C["nightSurface"] if dark else C["white"]
    accent = C["nightAccent"] if dark else C["vermilion"]
    parts = [f'<g transform="translate({x} {y}) scale({scale})">']
    for dx in range(0, 385, 24):
        for dy in range(0, 313, 24):
            parts.append(rect(dx, dy, 1.5, 1.5, border))
    parts.append(f'<path d="M72 88V156H192V228M312 88V156H192" fill="none" stroke="{border}" stroke-width="2"/>')
    parts.append(f'<path d="M72 88V156H192V228" fill="none" stroke="{accent}" stroke-width="2"/>')
    for dx, dy, name, changed in [(16, 30, "billing", True), (256, 30, "orders", False), (136, 228, "web", False)]:
        parts.append(rect(dx, dy, 112, 58, surface, 3, accent if changed else border))
        parts.append(label(name, dx + 16, dy + 35, 16, "mono", fg))
    parts += [rect(185, 149, 14, 14, accent), label("API changed", 94, 119, 11, "mono", accent), label("app affected", 211, 203, 11, "mono", secondary), '</g>']
    return "".join(parts)


def build_logos():
    for name, color in [("ink", C["ink"]), ("paper", C["paper"]), ("vermilion", C["vermilion"])]:
        save("logos", f"mark-{name}.svg", 32, 32, "Lemma scope mark", mark(size=32, color=color))
        save("logos", f"wordmark-{name}.svg", 194, 52, "lemma", label("lemma", 0, 41, 52, "medium", color))
        save("logos", f"lockup-{name}.svg", 270, 52, "Lemma", lockup(0, 4, 44, color, color))
    save("logos", "lockup-primary.svg", 270, 52, "Lemma", lockup(0, 4, 44, C["ink"], C["vermilion"]))
    save("logos", "lockup-reverse.svg", 270, 52, "Lemma", lockup(0, 4, 44, C["paper"], C["nightAccent"]))
    save("logos", "favicon.svg", 32, 32, "Lemma", rect(0, 0, 32, 32, C["vermilion"], 6) + mark(4, 4, 24, C["paper"]))
    save("logos", "avatar.svg", 512, 512, "Lemma avatar", rect(0, 0, 512, 512, C["vermilion"]) + mark(120, 120, 272, C["paper"]))
    save("logos", "avatar-dark.svg", 512, 512, "Lemma avatar, dark", rect(0, 0, 512, 512, C["ink"]) + mark(120, 120, 272, C["paper"]))


def build_campaigns():
    body = rect(0, 0, 1200, 630, C["paper"]) + lockup(64, 52, 38)
    body += label("CATCH BREAKING CHANGES BEFORE YOU MERGE", 64, 171, 11, "mono", C["muted"], 1)
    body += label("Move fast.", 60, 283, 108, "serif") + label("See what breaks.", 60, 382, 92, "italic", C["vermilion"])
    body += label("Clearer code changes. For people and agents.", 64, 449, 20) + label("Across services. Across repositories.", 64, 480, 20)
    body += diagram(758, 163, .94)
    body += line(64, 548, 1136, 548)
    body += label("LEMMA / RELEASE CANDIDATE", 64, 585, 11, "mono", C["muted"], .7) + label("CLI + MCP", 1023, 585, 12, "mono", C["muted"])
    save("marketing", "social-card.svg", 1200, 630, "Move fast. See what breaks. Lemma.", body, "Lemma checks service contracts across repositories before a change merges. Conceptual relationship diagram.")

    body = rect(0, 0, 1200, 630, C["ink"]) + lockup(64, 52, 38, C["paper"], C["nightAccent"])
    body += label("SMALL CHANGE. WIDER VIEW.", 64, 171, 11, "mono", C["nightMuted"], 1)
    body += label("Build with", 60, 288, 100, "serif", C["paper"]) + label("clarity.", 60, 391, 100, "italic", C["nightAccent"])
    body += label("Find what your service change breaks.", 64, 467, 21, "sans", C["paper"])
    body += diagram(758, 163, .94, True) + line(64, 548, 1136, 548, C["nightLine"])
    body += label("LEMMA / FOR PEOPLE + CODING AGENTS", 64, 585, 11, "mono", C["nightMuted"], .7)
    save("marketing", "social-card-dark.svg", 1200, 630, "Build with clarity. Lemma.", body)

    body = rect(0, 0, 1080, 1080, C["paper"]) + lockup(72, 64, 42)
    body += label("CONFIDENT CHANGES. CONNECTED CODE.", 72, 202, 13, "mono", C["muted"], 1)
    body += label("Your change", 68, 332, 122, "serif") + label("has company.", 68, 452, 122, "italic", C["vermilion"])
    body += label("Catch breaking changes before you merge.", 72, 521, 25)
    body += diagram(520, 607, 1.12)
    body += label("SEE THE CHANGE.", 72, 680, 12, "mono", C["muted"], .8)
    body += label("FOLLOW THE IMPACT.", 72, 720, 12, "mono", C["muted"], .8)
    body += label("TAKE THE NEXT STEP.", 72, 760, 12, "mono", C["muted"], .8)
    body += line(72, 966, 1008, 966) + label("LEMMA / RELEASE CANDIDATE", 72, 1012, 12, "mono", C["muted"], .5)
    save("marketing", "launch-square.svg", 1080, 1080, "Your change has company. Lemma.", body)

    body = rect(0, 0, 1500, 500, C["ink"]) + lockup(72, 54, 40, C["paper"], C["nightAccent"])
    body += label("Move fast. See what breaks.", 68, 247, 98, "serif", C["paper"])
    body += label("Catch breaking changes before you merge. For people and coding agents.", 72, 321, 24, "sans", C["nightMuted"])
    body += line(72, 408, 1428, 408, C["nightLine"]) + label("LEMMA / CLI + MCP", 72, 450, 12, "mono", C["nightMuted"], 1)
    body += mark(1280, 52, 148, C["nightAccent"])
    save("marketing", "profile-banner.svg", 1500, 500, "Lemma profile banner", body)

    body = rect(0, 0, 1280, 360, C["paper"]) + lockup(56, 46, 38)
    body += label("Move fast. See what breaks.", 52, 211, 83, "serif")
    body += label("Catch breaking changes across services. For people and coding agents.", 56, 278, 20)
    body += mark(1104, 47, 114)
    save("marketing", "readme-banner.svg", 1280, 360, "Lemma. Move fast. See what breaks.", body)

    body = rect(0, 0, 1920, 1080, C["paper"]) + lockup(96, 80, 44)
    body += label("Move fast.", 90, 449, 186, "serif") + label("See what breaks.", 90, 624, 166, "italic", C["vermilion"])
    body += label("CATCH BREAKING CHANGES BEFORE YOU MERGE", 96, 736, 18, "mono", C["muted"], 1)
    body += diagram(1220, 287, 1.4) + line(96, 960, 1824, 960)
    body += label("LEMMA / BRAND SYSTEM 1.0", 96, 1012, 15, "mono", C["muted"], 1)
    save("marketing", "presentation-cover.svg", 1920, 1080, "Lemma presentation cover", body)


def build_board():
    body = rect(0, 0, 1600, 1200, C["paper"]) + lockup(64, 52, 42)
    body += label("IDENTITY SYSTEM / 1.0", 1260, 84, 12, "mono", C["muted"], 1)
    body += line(64, 128, 1536, 128)
    body += label("Move fast.", 58, 286, 144, "serif") + label("See what breaks.", 58, 420, 127, "italic", C["vermilion"])
    body += label("Catch breaking changes before you merge.", 64, 486, 25)
    body += label("For people and coding agents.", 64, 522, 25)
    body += diagram(1050, 202, 1.12)
    body += line(64, 604, 1536, 604)
    body += label("01 / THE PALETTE", 64, 649, 11, "mono", C["muted"], 1)
    colors = [("Ink", "ink"), ("Paper", "paper"), ("Vermilion", "vermilion"), ("Forest", "forest"), ("Linen", "linen")]
    for i, (name, key) in enumerate(colors):
        x = 64 + i * 296
        body += rect(x, 680, 284, 154, C[key], 3, C["line"] if key == "paper" else None)
        body += label(name, x, 865, 17, "medium") + label(C[key], x, 891, 12, "mono", C["muted"])
    body += line(64, 932, 1536, 932)
    body += label("02 / THE TYPE", 64, 976, 11, "mono", C["muted"], 1)
    body += label("Instrument Serif", 62, 1062, 57, "serif") + label("Geist", 688, 1062, 49, "sans") + label("Geist Mono", 1060, 1062, 37, "mono")
    body += label("VOICE + CAMPAIGNS", 64, 1112, 11, "mono", C["muted"], .8) + label("INTERFACES + BODY", 688, 1112, 11, "mono", C["muted"], .8) + label("CODE + EVIDENCE", 1060, 1112, 11, "mono", C["muted"], .8)
    save("exports", "brand-board.svg", 1600, 1200, "Lemma brand identity", body, "Warm paper, ink, vermilion, forest and linen. Instrument Serif display type, Geist interface type, Geist Mono for code. A scope mark and orthogonal relationship drawings.")


def build_fonts():
    sources = []
    for path in sorted((ROOT / "fonts").glob("*.ttf")):
        font = TTFont(path)
        font.flavor = "woff2"
        font.save(path.with_suffix(".woff2"))
        sources.append(f"- `{path.name}` — SHA-256 `{hashlib.sha256(path.read_bytes()).hexdigest()}`")
    (ROOT / "fonts" / "CHECKSUMS.md").write_text("# Font source checksums\n\nDownloaded 2026-11-07 from the Google Fonts source repository. WOFF2 files are compressed from these unmodified TTF sources.\n\n" + "\n".join(sources) + "\n")


if __name__ == "__main__":
    build_fonts()
    build_logos()
    build_campaigns()
    build_board()
    print("Built fonts, 14 logo assets, 6 campaign assets, and the brand board.")

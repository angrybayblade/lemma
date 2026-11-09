# Fonts

The kit bundles TTF source fonts and compressed WOFF2 web fonts. No third-party font requests are made by the visual guide.

| Family | Included files | Upstream |
| Barlow Condensed | SemiBold 600; TTF + WOFF2 | [Google Fonts](https://github.com/google/fonts/tree/main/ofl/barlowcondensed) |
| --- | --- | --- |
| Instrument Serif | Regular and Italic; TTF + WOFF2 | [Instrument](https://github.com/Instrument/instrument-serif) |
| Geist | Variable weight 100–900; TTF + WOFF2 | [Vercel](https://github.com/vercel/geist-font) |
| Geist Mono | Variable weight 100–900; TTF + WOFF2 | [Vercel](https://github.com/vercel/geist-font) |

All four use SIL Open Font License 1.1. The original notices are preserved in `licenses/`; include them when redistributing the font files. WOFF2 compression preserves font names and glyph designs.

Source files downloaded 2026-11-07:

- [Instrument Serif Regular](https://github.com/google/fonts/blob/main/ofl/instrumentserif/InstrumentSerif-Regular.ttf)
- [Instrument Serif Italic](https://github.com/google/fonts/blob/main/ofl/instrumentserif/InstrumentSerif-Italic.ttf)
- [Geist variable](https://github.com/google/fonts/tree/main/ofl/geist)
- [Geist Mono variable](https://github.com/google/fonts/tree/main/ofl/geistmono)

See [checksums](CHECKSUMS.md) for the exact bundled TTF content. `tokens.css` registers all four faces. The variable sans and mono files contain their full weight ranges; use 400–550 for normal brand work.

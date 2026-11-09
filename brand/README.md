# Lemma brand kit

**Accepted direction: [Aicher / Unigrid / Suisse](DIRECTION.md).** Aicher's Munich standards govern the graphic language, Vignelli's Unigrid governs layout, and Suisse is the selected type family. The current palette uses the owner's faded metallic refinement: Steel, Silver, Ink, muted Copper, and supporting Sage.

**Design system:** kept out of the repository while it is out of date. The site tracks the files it needs in `website/design-system/`.

The editorial, industrial, and Swiss/architectural boards and assets are earlier explorations. The landing page now uses the faded metallic system, agent-first messaging, and animated SVG workflows. The documentation below describes the original editorial kit.

**Move fast. See what breaks.**

Lemma checks how services connect, so people and coding agents can catch breaking changes before code merges. The identity is expressive, clear, and precise: warm paper, dark ink, vermilion, a geometric scope mark, and editorial typography.

## See the system

Open `index.html` in a browser. For the local preview, run this from the repository root:

```sh
python3 -m http.server 8765 --bind 127.0.0.1
```

Then open [the brand guide](http://127.0.0.1:8765/brand/). This is a local review guide; it has no external services, analytics, or build step. All fonts are included. Light and dark previews, copyable colors and commands, field validation, and keyboard-operated tabs work locally.

## What is included

| Location | Contents |
| --- | --- |
| [DIRECTION.md](DIRECTION.md) | Accepted references, proposed application rules, and font requirements for the next design pass |
| [BRAND.md](BRAND.md) | Positioning, logo rules, color, accessibility, typography, composition, and motion |
| [MESSAGING.md](MESSAGING.md) | Headlines, short descriptions, launch copy, and claim boundaries |
| [logos/](logos/) | Primary, reverse, and monochrome lockups; wordmarks; marks; favicon; avatars |
| [marketing/](marketing/) | Six vector campaign layouts with outlined type |
| [exports/](exports/) | Upload-ready PNGs, favicon sizes, and a brand board |
| [fonts/](fonts/) | Local TTF and WOFF2 files, source checksums, and OFL licenses |
| [tokens.json](tokens.json) | Authoritative values for color, type, spacing, shape, motion, and themes |
| [tokens.css](tokens.css) | Generated web fonts and CSS custom properties |
| [components/](components/) | Framework-independent CSS components and usage documentation |
| [scripts/](scripts/) | Reproducible token, vector, and raster generation |
| [VALIDATION.md](VALIDATION.md) | Browser, accessibility, link, and export checks |

## Use the components

```html
<link rel="stylesheet" href="/brand/tokens.css">
<link rel="stylesheet" href="/brand/components/components.css">
<section class="lm-surface" data-lm-theme="light">
  <h1 class="lm-display">Move fast.<br>See what breaks.</h1>
  <a class="lm-button" href="/docs/quickstart">Get started</a>
</section>
```

Preserve the `fonts/` directory relative to `tokens.css`. If relocating the CSS, update its font URLs. Use `data-lm-theme="dark"` on the surface or document root to activate dark tokens. The guide's own CSS and JavaScript are review tooling, separate from the reusable components.

## Assets and sizes

| Asset | Dimensions | Use |
| --- | --- | --- |
| `social-card` / `social-card-dark` | 1200 × 630 | Open Graph, link previews, social launch |
| `launch-square` | 1080 × 1080 | Square posts |
| `profile-banner` | 1500 × 500 | Profile headers; check the platform's crop |
| `readme-banner` | 1280 × 360 | Repository or documentation header |
| `presentation-cover` | 1920 × 1080 | Presentation title slide or video cover |
| `avatar` / `avatar-dark` | 512 × 512 | Organization or project avatar |
| `brand-board` | 1600 × 1200 | Shareable identity overview |
| `favicon-16`, `favicon-32`, `apple-touch-icon` | 16, 32, 180 square | Browser / device icons |

Use the PNGs on platforms that do not accept SVG. SVGs are standalone vectors with outlined typography and no font installation requirement. Copy changes happen in `scripts/build_assets.py`; regenerate rather than editing glyph paths. These are brand compositions, not screenshots of a shipped graphical application.

## Regenerate

Token CSS uses only Python's standard library:

```sh
python3 brand/scripts/build_tokens.py
```

Vector assets and WOFF2 files require `fonttools` and `brotli` in your chosen environment:

```sh
python3 -m venv /tmp/lemma-brand-build
/tmp/lemma-brand-build/bin/pip install fonttools brotli
/tmp/lemma-brand-build/bin/python brand/scripts/build_assets.py
```

PNG export uses `playwright-core` and an installed Chrome-compatible browser. Keep dependencies outside the Rust workspace:

```sh
npm install --prefix /tmp/lemma-brand-build playwright-core
PLAYWRIGHT_MODULE=/tmp/lemma-brand-build/node_modules/playwright-core \
CHROME_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
node brand/scripts/export_assets.cjs
```

Exporting does not require a web server. No Rust sources or runtime dependencies are changed by this kit.

## Sources and rights

[Instrument Serif](https://github.com/Instrument/instrument-serif), [Geist and Geist Mono](https://github.com/vercel/geist-font) are distributed under the SIL Open Font License; their licenses are included. Font source files come from the [Google Fonts repository](https://github.com/google/fonts). See [font provenance](fonts/README.md).

The identity is a proposed Lemma direction, version 1.0, dated 2026-11-07. Brand rights follow the project owner's decisions; this kit does not assign a software license or change the repository's licensing. Entail remains the proposed hosted product described in the project's naming notes.

# Lemma — Swiss systems

**Earlier exploration · Version 3.0 · 2026-11-07**

The accepted direction is now [Aicher / Unigrid / Suisse](../DIRECTION.md). This board and its assets are retained as an earlier prototype.

**Move fast. With precision.**

This direction combines Swiss typographic discipline with architectural systems drawing. Precision is expressed through alignment, proportion, consistent line weights, and a clear relationship between a local change and its wider context. Trust comes from plain language, inspectable findings, and honest uncertainty.

## Palette

| Color | Hex | Use |
| --- | --- | --- |
| Paper | `#F0F1EB` | Main, neutral stock surface |
| White | `#FAFBF6` | Subtle distinct surface |
| Wash | `#E5E8E0` | Sections that share a purpose |
| Graphite | `#262B28` | Primary text and structure |
| Secondary | `#606960` | Supporting copy |
| Rule | `#C4CAC0` | Quiet separators and construction lines |
| Control | `#788277` | Meaningful control boundaries |
| Deep red | `#A9342E` | A precise intervention: changed field, affected caller, primary action |
| Red hover | `#8A2924` | Action feedback |
| Drawing | `#929D91` | Architectural structure |
| Verified | `#496A50` | Reserved for actual satisfied outcomes |

Keep most of the composition neutral. Red should name a particular point or action. Do not add red to every rule or heading. Use opaque text and verify contrast when changing a surface or texture.

## Typography

Geist is the primary family for both display and body. Use sentence case, 400–450 display weight, 400 body weight, and 500 for compact actions. Large headings use tight but consistent tracking, approximately -0.05 to -0.068em. The display voice is direct and composed rather than condensed or emphatic.

Geist Mono is reserved for actual commands, fields, symbols, and source details. Most section indexes and navigation use the sans family. Avoid filling the composition with capitalized technical metadata.

Font files and their original SIL Open Font License notices remain in [fonts](../fonts/README.md). No external font requests are needed.

## Layout and material

Use a clear grid with generous margins. Align the page's major reading edges and reuse the same spacing scale. Organize related content with placement and fine rules. Buttons have small 2px corners, native focus behavior, and a minimum 44px target.

The surface has a very faint uncoated-stock grain. It should be barely perceptible at normal reading distance and should never add visible scan lines or obscure type. The framework's credibility should come from how clearly the composition works.

Keep architectural drawings as drawings: fine lines, open space, repeated modules, and distinct layers. Avoid rendered hardware, glows, gauges, console chrome, status dashboards, and invented instrumentation. Dense context should remain understandable rather than become decoration.

## The relationship drawing

Two registered layers represent related repositories. A small location in one layer is connected to a caller in the other. Red identifies the exact relationship under discussion; the surrounding structure stays neutral.

The landing-page example follows the repository quickstart: Billing removes `receipt_url`, and Web still reads it. Labels 01 and 02 refer to the source and the caller. The illustration is explicitly conceptual, not a screenshot or a real analysis result.

Before/after controls change the statement and the highlighted relationship. They behave as ordinary labeled buttons. There is no decorative looping animation. On mobile, the drawing fits the available width; the factual explanation remains readable HTML below it.

## Voice

Lead with what Lemma does:

> Lemma checks the contracts between your services and shows the code a change breaks.

Then add the scope and users:

> Across repositories. Before you merge. CLI for you. MCP for your agent.

Use brief, concrete sentences. Keep uncertainty visible. Do not claim universal correctness, unlimited scale, complete coverage, certification, or unmeasured performance. “With precision” is the design and experience intent; it does not override the tool's documented limits.

## Components

Import [tokens.css](tokens.css), then [components.css](components.css), inside a `.lm-precision` surface. These framework-independent primitives cover actions, headings, commands, findings, and separators. They use the same palette as the landing page.

- `.lm-button`: a native action, or an anchor for navigation.
- `.lm-button--secondary`: a lower-emphasis outlined action.
- `.lm-heading`: a sentence-case display heading; choose a semantic heading level independently.
- `.lm-command`: selectable, wrapping command text without decorative terminal chrome.
- `.lm-finding`: a precise finding statement with actual source context.
- `.lm-rule`: a subtle separator, not a meaningful input boundary.

Use native `disabled` on an unavailable button. Clipboard actions need an accessible name and feedback. The landing page implements copy feedback, navigation, and the illustrated state in `website/src/app.js`.

## Source and use

- [tokens.json](tokens.json): authoritative values.
- [tokens.css](tokens.css): generated variables and local font declarations.
- [logos](logos/): the retained scope mark in Deep Red and Graphite.
- [Visual board](index.html): a compact view of the direction.
- [Sharing artwork](marketing/social-card.svg): outlined SVG master.
- [Landing-page implementation](../../website/README.md): responsive layout and behavior.

Generate assets using the existing fonttools environment:

```sh
python3 brand/scripts/build_precision.py
python3 website/scripts/build_system_map.py
```

`brand/scripts/export_precision.cjs` exports PNG previews using a local Chrome-compatible browser. Then run `node website/scripts/sync_brand.mjs` and `node website/scripts/build.mjs` to apply the assets to the page. The prior editorial and industrial directions remain as historical explorations.

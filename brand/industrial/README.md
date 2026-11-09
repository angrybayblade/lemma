# Lemma — industrial precision

**Earlier exploration · Version 2.0 · 2026-11-07**

The accepted direction is [Aicher / Unigrid / Suisse](../DIRECTION.md). This industrial exploration and the later Swiss/architectural prototype are retained for reference.

**Move fast. Know the impact.**

This direction expresses software work as precise intervention in a large connected system. A small local change can affect code elsewhere. The visual language draws from instrument panels, machined surfaces, circuit schematics, and engineering drawings.

## Palette

| Color | Hex | Purpose |
| --- | --- | --- |
| Graphite | `#111719` | Main canvas and primary text on light surfaces |
| Recess | `#0C1113` | Deep map and command surfaces |
| Panel | `#1A2326` | Distinct dark instruments and inspection plates |
| Alloy | `#DDE3DF` | Brushed light surfaces; secondary dark text |
| White | `#F1F3EE` | Primary dark-surface text |
| Steel | `#A4B3B6` | Diagram labels and measured secondary emphasis |
| Signal orange | `#FF6B35` | Inspected change, affected connection, primary action |
| Dark signal | `#983616` | Accessible accent text on Alloy |
| Verified | `#ACCA91` | Reserved for real satisfied outcomes |
| Caution | `#E5C05B` | Reserved for real warnings |

Graphite and Alloy establish the physical material. Signal orange marks the exact point that matters. Keep secondary relationships in steel so the affected path remains unambiguous. Use Graphite text on bright-orange actions. Use Dark Signal for small accent text on Alloy; bright Signal is too light there.

This is an industrial visual reference, not a claim of safety certification. Avoid invented instrument readings, performance metrics, compliance marks, or precision figures.

Approved opaque text pairs measure 16.20:1 for White on Graphite, 7.20:1 for Muted on Graphite, 6.38:1 for Graphite on Signal, and 5.61:1 for Dark Signal on Alloy. Recheck contrast when changing opacity or placing text over imagery.

## Typography

- **Barlow Condensed 600:** short uppercase headlines. Compact, direct, and structural; avoid italic display type.
- **Geist 400–550:** human explanations, navigation, and buttons. Body copy remains in sentence case.
- **Geist Mono 400:** source fields, actual commands, diagram labels, and section indexes. Use it sparingly outside technical content.

The display face is from the [Google Fonts Barlow Condensed source](https://github.com/google/fonts/tree/main/ofl/barlowcondensed). Its [OFL license](../fonts/licenses/BarlowCondensed-OFL.txt) is included, alongside Geist's licenses. The local TTF and WOFF2 live in `../fonts/`.

## Material and geometry

Texture should suggest machining grain, not obscure the information. `texture.svg` supplies a deterministic fractal grain; combine it with very faint linear brushing. Alloy sections use a subtler multiply treatment. Keep text opaque and clearly separated from background texture.

Use thin construction rules, corner registration marks, chamfered schematic plates, and square contacts. The marks should support a real frame, alignment, layer, or connection. Avoid decorative gauges, endless dashboards, or random code rain.

Dense surrounding modules communicate complexity. One clearly highlighted path communicates precision. In the landing-page illustration, Billing removes `receipt_url` and the Web caller still reads it. The wider system is explicitly illustrative; it does not pretend to be a shipped visual application or a real analysis run.

## Speed and trust

Condensed typography, directional traces, and short paths to action convey speed. A single path draws when a visitor switches to the breaking-change example. There is no looping motion. Reduced-motion mode removes the trace animation.

Trust comes from source-oriented explanations and visible uncertainty. Keep the product's release-candidate status accessible. Do not translate “at system scale” into an unmeasured guarantee about arbitrary codebases.

## Files

- [tokens.json](tokens.json): source palette, fonts, spacing, and theme values.
- [tokens.css](tokens.css): generated CSS variables and local font faces.
- [components.css](components.css): reusable industrial actions, headings, commands, inspection surfaces, and rules.
- [logos/](logos/): the retained scope mark with Signal/Graphite coloring.
- [marketing/social-card.svg](marketing/social-card.svg): standalone outlined social artwork.
- [index.html](index.html): compact visual identity board.
- [Landing-page source](../../website/README.md): the complete application of this direction.

Regenerate token CSS, logo variants, and vector campaign artwork with:

```sh
python3 brand/scripts/build_industrial.py
```

This uses the same `fonttools` environment described in the main brand kit. `brand/scripts/export_industrial.cjs` renders the industrial social PNG and a brand-board preview using a local Chrome-compatible browser. The original editorial exploration is retained in the parent directory for reference; this industrial direction supersedes it for the landing page.

## Component integration

Import `tokens.css`, then `components.css`. Wrap content in `.lm-industrial`. Use `.lm-button` for an action (or an anchor for navigation), `.lm-button--secondary` for lower emphasis, `.lm-heading` for display type, `.lm-index` for supporting metadata, `.lm-command` for actual selectable commands, and `.lm-inspection` for a finding with an explicit statement.

Choose a real heading level. Use a native disabled button for unavailable actions. Any copy action needs a meaningful accessible name and a status message; the landing page implements those behaviors in `website/src/app.js`. Decoration does not replace field labels, outcome text, or source context.

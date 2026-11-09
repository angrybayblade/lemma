# Lemma identity system

This document records the original editorial direction. The active visual direction is [Swiss systems / v3](precision/README.md).

Version 1.0 · 2026-11-07 · Direction: expressive precision

## The idea

**Move fast. See what breaks.**

A change can look correct in one repository while breaking another. Lemma makes those connections visible before the change merges. People and coding agents can move forward with clearer context.

The brand should communicate the product in seconds. Lead with the change and what it affects. Explain implementation only when the reader needs it.

| Quality | What a person should feel | How the design earns it |
| --- | --- | --- |
| Trust | “I can understand what this is telling me.” | Clear language, visible scope, honest outcomes, consistent interactions |
| Precision | “I know where to look.” | Direct labels, aligned type, crisp geometry, specific source context |
| Speed | “I can get to the next step.” | Short headlines, obvious actions, easy scanning, restrained motion |
| Expression | “I will remember this.” | Oversized editorial type, vermilion, a compact mark, confident compositions |

The ambition is clarity as software grows: from a person's next change to a team's connected repositories. “At any scale” is a design ambition, not a performance guarantee.

## Position and audience

Plain description: **Lemma catches breaking changes between services before code merges.**

Expanded description: Lemma reads the repositories in a system, checks the contracts between services, and shows affected code. It also gives people and coding agents ways to navigate code through the CLI and MCP.

The primary audience is developers and teams changing software that spans services or repositories. Coding agents are equally important users of the underlying tool. Speak to the person choosing and understanding the tool; show what an agent can do with it.

Keep Lemma distinct from Entail. Lemma is the reusable framework named in the repository. Entail is a potential hosted product. Do not advertise hosted workspaces, sign-in, pricing, or team administration as current Lemma features.

## Logo

The **scope mark** consists of an L-shaped frame and a smaller offset corner, on a 32-unit square. It suggests a local view within a larger system. The wordmark is custom-spaced, outlined Geist at weight 550.

- Use `lockup-primary.svg` on Paper or White.
- Use `lockup-reverse.svg` on Ink; its accent is lighter for dark surfaces.
- Use `lockup-ink.svg` or `lockup-paper.svg` for single-color applications.
- Use the mark for compact contexts once Lemma's name is established. Use the supplied avatar for profile images and `favicon.svg` for the browser.
- Clear space: at least 8 units on every side for a 32-unit mark canvas, or 25% of the rendered mark canvas width. For a lockup, base this on the symbol's canvas width, not the whole lockup.
- Minimum: standalone mark 20px canvas; full lockup 120px wide. Use the dedicated favicon at 16px.
- Preserve proportions and solid fills. Keep the supplied wordmark spacing. Avoid rotations, gradients, outlines, shadows, or adding containers to the master mark.

The SVG logo type is outlined for consistent rendering. Source text, geometry, and spacing are maintained in `scripts/build_assets.py`.

## Color

| Primitive | Hex | Role |
| --- | --- | --- |
| Ink | `#20241F` | Primary text, dark campaigns, structural emphasis |
| Paper | `#F5F2EB` | Main light surface |
| White | `#FFFEFA` | Raised or distinct light surfaces; text on Vermilion |
| Linen | `#EAE6DC` | Subtle surfaces and grouping |
| Vermilion | `#BA432B` | Brand signature, primary actions, active relationships |
| Vermilion hover | `#9E3623` | Action hover; violated text on a soft surface |
| Forest | `#355C48` | Satisfied outcomes |
| Muted | `#65685F` | Secondary copy |
| Line | `#D7D5CB` | Decorative rules and separators |

A suggested light campaign balance is 70% Paper, 20% Ink, 10% Vermilion. It is a composition guide, not a fixed formula. A short, expressive panel can be entirely Vermilion. Keep Forest out of decorative campaigns so it retains meaning in outcomes.

Use semantic variables (`--lm-fg`, `--lm-accent`, `--lm-violated`, etc.) inside components. Primitive colors are for deliberately fixed campaign compositions. Dark mode uses Night `#191D19`, Night Surface `#242A24`, Paper text, and a light Vermilion `#F29A7D`. Do not reuse light-theme muted colors on dark surfaces.

### Contrast and meaning

Small text needs at least 4.5:1 contrast; large text needs at least 3:1 under [WCAG's contrast guidance](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html). The kit's approved semantic text pairings meet the small-text threshold. Decorative Line is not an input boundary: use `--lm-controlBorder` for controls.

| Text / surface | Ratio |
| --- | --- |
| Ink / Paper | 14.08:1 |
| Muted / Paper | 5.08:1 |
| Vermilion / Paper | 4.78:1 |
| White / Vermilion | 5.30:1 |
| Forest / Forest Soft | 6.21:1 |
| Paper / Night | 15.26:1 |
| Night Muted / Night Surface | 7.23:1 |
| Light Vermilion / Night | 7.87:1 |

Ratios use sRGB relative luminance and the opaque token values. Changing a background, opacity, or image requires another contrast check. These values are not a full conformance certification.

Outcomes always use a word and, where useful, a symbol:

| Outcome | Symbol | Meaning |
| --- | --- | --- |
| Satisfied | ✓ | This evaluated requirement is satisfied for its inputs |
| Violated | × | The evaluation found a violation |
| Unknown | ? | The available evidence did not establish an outcome |
| Stale | ◷ | The evidence no longer matches the current inputs |

A satisfied requirement does not mean the whole change is safe. Do not turn unknown, stale, or unsupported information into a success indicator.

## Typography

| Family | Role | Weights | Fallback |
| --- | --- | --- | --- |
| Instrument Serif | Campaign headlines, major editorial headings, short pull quotes | 400 regular and italic | Georgia, serif |
| Geist | Body, product UI, navigation, wordmark | 400 body, 500 actions, 550 titles | System sans-serif |
| Geist Mono | Code, commands, source locations, technical metadata | 400, 500 | SFMono-Regular, Consolas, monospace |

Use sentence case. A headline should usually fit in 3–8 words. Reserve italic for one phrase rather than a whole page. Keep paragraphs in Geist; long passages of display serif or monospaced body text slow scanning.

The base type scale is 12 / 14 / 16 / 18 / 24 / 32 / 48 / 72 / 96px, stored in rem. Body text starts at 16px with 1.55–1.7 line height. Display type uses approximately 1.0–1.1 line height and -0.025 to -0.035em tracking. Use 500–550 weights for compact interface headings. Marketing metadata can be smaller when it is supplementary; essential instructions and controls must stay readable at their actual output size.

Local WOFF2 fonts use `font-display: swap`. TTFs are also included for design applications. Licenses and provenance are in `fonts/`.

## Composition and visual language

Lead with one expressive headline, one plain explanation, and one obvious next step. Product marketing should answer “what does it do?” before describing how it works.

Use a 4px base spacing system: 4, 8, 12, 16, 24, 32, 48, 64, 96. Desktop layouts cap around 1200px; prose around 672px. Reflow into a single column when content becomes cramped. Preserve whitespace and reading order before shrinking type.

Rules and orthogonal connectors give the system precision. Small squares represent points of connection. A faint grid can support diagrams; it should never compete with labels. Use 1–2px lines and real labels. Highlight the one relationship being discussed, and keep unrelated structure neutral.

Marketing diagrams are conceptual illustrations, not product screenshots. The provided example uses billing, orders, and web. For a real result, display its own source locations, revisions, and coverage. Avoid decorative dashboards, invented customer logos, and fabricated metrics.

Use 4px corners for buttons and small surfaces, 8px for terminals and larger components. Reserve pills for truly compact categorization. Let typography, spacing, and contrast carry the design; shadows are exceptional.

## Components and motion

Components are framework-independent CSS. [Component documentation](components/README.md) defines the HTML, states, and behavioral requirements. Components should remain legible in both themes.

Keep primary actions direct: “Get started,” “Read the docs,” “Copy command.” Avoid “Unlock,” “Supercharge,” and generic “Learn more” when a concrete action is available.

Use 120ms for hover/focus feedback and 180ms for a state transition. No looping “speed” animation is needed. Motion should clarify which relationship or state changed. Respect `prefers-reduced-motion`; the tokens reduce transitions to zero and the guide disables smooth scrolling.

## Voice and claims

Be clear, specific, and brief. Describe the change, the affected code, and the next step. First mention: “breaking changes between services.” Follow with “contracts,” “snapshots,” or “semantic providers” only when explaining a relevant technical concept.

Do not claim universal correctness, complete coverage, zero incidents, arbitrary scale, or a benchmark speedup without scoped evidence. The README's benchmark figures have explicit repositories, samples, and limits; keep that context if using them. Current public copy should retain “release candidate” where maturity matters.

See [MESSAGING.md](MESSAGING.md) for ready-to-use language. This system follows the repository's [introduction](../docs/introduction.md), [quickstart](../docs/quickstart.md), and [naming notes](../.specs/README.md).

# Lemma — accepted brand direction

Selected 2026-11-07. The [design system v1](system/README.md) applies this brief with tokens, original symbols, layout rules, reusable components, and a visual guide. The landing page now applies the faded metallic system to an agent-first introduction with animated SVG workflows. Older boards remain earlier explorations. Licensed Suisse files are pending; the system labels its fallback preview explicitly.

## Selected references

| Area | Reference | What it governs |
| --- | --- | --- |
| Aesthetic, visual noise, color roles | Otl Aicher, Munich 1972, with the owner's faded metallic palette refinement | Clear hierarchy, restrained detail, consistent visual roles |
| Visual elements | Aicher's Munich design standards | Pictograms, arrows, line construction, repeatable graphic relationships |
| Layout | Massimo Vignelli, Unigrid | Modular composition, recurring alignments, margins, captions, bars, and dividers |
| Typography | Suisse, Swiss Typefaces | The type family throughout the identity |

The references above are selected. The application rules below are the proposed translation for Lemma, rather than claims about the historical manuals.

## Communication

Lead with what a change breaks, for people and agents changing connected software:

> Move fast. See what breaks.

The core capabilities are checks of service contracts and declared requirements, cross-service/repository impact analysis, and checker-backed behavior simulations. Code discovery supports that larger development loop; it is not the product's leading proposition. Keep the broader purpose clear:

> Lemma gives agents a system model and executable evidence to see what a change breaks and check the fix.

Show the loop: declare requirements, analyze impact, simulate behavior, and inspect the evidence. Use a behavior replay to demonstrate something a static contract alone cannot establish. Keep copy concise and preserve the approved metal-diagram / focused-explanation layout. Verification claims apply to modeled relationships, declared properties, checked inputs, and executed scenarios; they do not establish arbitrary program correctness. Claims about performance and coverage must match the product documentation.

## Color and surface

Use the owner's faded metallic palette: Steel, Silver, and Ink as the core; muted Copper for emphasis; Sage for supporting outcomes. Unknown remains neutral. This refinement supersedes the earlier bright blue, green, and orange interpretation. Aicher's discipline of clear color roles remains relevant; the [IDZ reference](https://www.otlaicher.de/en/articles/butterflies-nasturtiums-roses-and-chromatic-greys/) describes his use of color for wayfinding.

The v1 system defines a small set of repeatable digital color roles and checked text pairings. Keep outcome semantics explicit; pair a state color with a label or symbol. These values are Lemma's digital translation of the reference. Previous Paper/Graphite/Deep Red tokens do not define this direction.

Use generous clear space and matte painted-metal color fields, following the owner's subsequent material reference. Fine grain and a shallow dimple pattern add tactile detail. Keep controls and reading surfaces flat, with crisp live type and symbols. Let repetition, color, and composition provide visual richness.

## Graphic elements

- Construct original Lemma pictograms on a common module, with consistent line weight, angles, spacing, and optical size. Use a limited vocabulary for a change, a relationship, an affected caller, and a check.
- Use arrows and paths to make reading order and cause-and-effect visible. Start with one relationship; introduce surrounding modules only when they help explain scope.
- Keep the construction grid mostly invisible. Show grid marks or reference points where they clarify alignment in a diagram. A ruler needs an actual dimension or scale to communicate.
- Give each line a stable purpose: connection, division, or emphasis. Use the same purpose consistently across assets.
- Use repeatable symbols and graphic proportions to convey precision. Use directional composition and short sequences to convey speed. Use clear labels and inspectable examples to build trust.

The [Munich standards manual](https://niggli.ch/en/products/richtlinien-und-normen-fur-die-visuelle-gestaltung) is the graphic reference. The resulting symbols should describe Lemma's software concepts. Earlier console frames, gauges, glowing traces, and exploded architectural drawings are not part of this brief.

## Layout

Treat Unigrid as a system of relationships that works across landing pages, documentation, and marketing formats. The [Vignelli Center](https://www.rit.edu/vignellicenter/vignelli-public-graphics) documents the program; the [National Park Service](https://www.nps.gov/subjects/oralhistory/podcasts-episode-4.htm) describes how it organizes titles, illustrations, text, margins, captions, and dividers.

Use a shared column module, consistent gutters, a baseline rhythm, and recurring caption positions. Let headings, diagrams, and text span different multiples of that module. Establish a few deliberate compositions, including an asymmetric lead composition and compact information rows. Reflow these relationships for mobile in the intended reading order.

Group information through alignment, spacing, color fields, and rules. Define these relationships before designing individual components. The page should explain the product quickly, with one main idea per section.

## Typography

The selected family is [Suisse](https://www.swisstypefaces.com/fonts/suisse/). Proposed starting point: Suisse Int'l Regular and Medium for headlines, body, navigation, and controls; Suisse Int'l Mono Regular for actual commands and code. These subfamilies and weights are recommendations within the selected family.

Use sentence case, flush-left text, a small number of sizes, and deliberate line breaks. Keep paragraphs short and readable. Use weight and placement to establish hierarchy. Commands and code should remain selectable text.

Swiss Typefaces supplies WOFF2 for web use. No Suisse files were found in the workspace or the local font folders checked on 2026-11-07. Licensed webfont files are needed for implementation. Existing Geist and Instrument Serif files belong to earlier explorations.

## Application status

The [visual guide](system/index.html) demonstrates the foundations and component states. The [landing page](../website/README.md) applies them to agent-driven development with moving paths, scroll-based workflow scenes, a pause control, and reduced-motion support. Install licensed Suisse fonts before final typography and wordmark approval.

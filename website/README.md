# Lemma website

The approved landing page and a design-system-compatible documentation site share the same static build. The landing page's visuals and animations are preserved; its documentation links lead to `/docs/`.

## Documentation site

- All 27 canonical Markdown files in the repository's `docs/`, plus the agent-skills and editor READMEs, render as static pages. The introduction is the entry point, at `/docs/`.
- `src/docs/navigation.mjs` controls groups, labels, and reading order. The build rejects unlisted documentation files.
- `scripts/build_docs.mjs` renders headings, stable anchors, tables, file-labeled examples, local links, previous/next navigation, and the search index. Canonical Markdown is unchanged.
- `src/docs/docs.css` uses the existing design-system tokens and fonts. It is separate from landing-page styles. Reading surfaces stay clear, with muted code backgrounds and restrained copper navigation marks.
- `src/docs/docs.js` handles local search, keyboard shortcuts, native mobile navigation, active page outlines, and exact code copying. Article content and links render without JavaScript.
- `content/` is a generated source snapshot used to reproduce the Site build outside the Rust repository. Build from the root checkout to refresh it. `content/source.json` records the latest revision affecting the canonical documentation, so unrelated website commits do not change the generated source links.
- `vendor/marked/` pins Marked 18.1.3 and its MIT license. The parser runs only at build time; the docs have no runtime dependency or external search service.

Build and check from the Lemma repository:

```sh
node website/scripts/build.mjs
python3 website/scripts/check_docs.py
```

From the isolated Site checkout, use `node scripts/build.mjs` and `python3 scripts/check_docs.py`. The source snapshot makes the build self-contained. Preview `dist/` with any static HTTP server; open `/docs/`.

Search opens with the header button, `/`, or Command/Ctrl K. Arrow Down moves into results; Escape closes the dialog. Copy controls preserve the exact displayed example, including indentation. Small screens use native navigation and page-outline disclosures. Text uses Geist and Geist Mono until licensed Suisse files are supplied.


An agent-first introduction to Lemma, using the faded metallic design system: Steel, Silver, Ink, muted Copper, Geist typography, and original geometric SVGs. Text uses Geist and Geist Mono until licensed Suisse files are supplied.

The page leads with **Move fast. See what breaks.** Its core capabilities are checks of service contracts and declared requirements, cross-service/repository impact analysis, and checker-backed behavior simulations. Code discovery supports that broader workflow. The design-system guide remains available at `/design-system/`.

## Build and preview

The static site has no package or framework runtime dependencies:

```sh
node website/scripts/build.mjs
python3 -m http.server 8765 --bind 127.0.0.1 --directory website/dist
```

Open `http://127.0.0.1:8765/`. The Site source builds independently from its vendored design system.

## Source

- `src/index.html`: content, semantic structure, metadata, and SVG placement markers.
- `src/styles.css`: responsive page composition, transitions, moving paths, and motion preferences.
- `src/app.js`: pause/play, heading-based scene selection with a 20px deadband, sticky-stage positioning, dock navigation and tooltips, copy feedback, and the behavior replay.
- `src/diagrams-webgl.js`: shared dependency-free WebGL materials, configured change geometry, single-canvas blending, rendering lifecycle, and context recovery.
- `assets/change-story.json`: canonical workspace, proposed patch, repository identities, behavior cases, and requirement.
- `src/change-story.js`: generated browser data for that same example.
- `scripts/build_change_story_art.py`: config-driven SVG labels, fallback geometry, and browser-data generation.
- `src/hero.svg`, `workflow.svg`, `verify.svg`: original illustrations embedded at build time.
- `scripts/build_landing_art.py`: deterministic SVG and sharing-artwork generator.
- `scripts/sync_brand.mjs`: sync the brand system and standalone mark. Needs a local `brand/` folder, which is not in the repository.
- `scripts/build.mjs`: assemble public output under `dist/`.
- `design-system/`: the files of the design system the site needs. The rest, and `brand/`, are ignored for now.
- `assets/social-card.svg`, `.png`: page-specific sharing artwork.
- `scripts/export_previews.cjs`: desktop, full-page, mobile, and sharing-image exports.

The earlier architectural illustration and its generator are retained as exploration source; the current build uses the three new illustrations.

## Updating the system or artwork

From the repository root:

```sh
python3 website/scripts/build_landing_art.py
node website/scripts/build.mjs
```

After starting the local preview, render images using an external Playwright installation and local Chrome:

```sh
PLAYWRIGHT_MODULE=/absolute/path/node_modules/playwright-core \
  node website/scripts/export_previews.cjs
node website/scripts/build.mjs
```

Set `CHROME_PATH` for another installed Chrome-compatible binary or `LEMMA_PREVIEW_URL` for another local origin. The sharing PNG is copied into the final build, so rebuild after exporting. Font installation and token changes should be made in the canonical brand system, then synchronized here.

## Page hierarchy

The landing page uses three visual levels: a display headline for the main promise; one shared scale for section and step headings; and a consistent reading scale for copy, labels, and controls. The product story uses a 7:5 split: the narrative stays on the left and one continuous animation stage stays on the right. At 850px and below the same stage pins above the text. Alignment and spacing carry most of the structure.

The hero captions and status strip are removed. Repeated section counters, diagram headings, command callouts, and the oversized footer statement are also removed. Each explanatory block pairs its headline with explanatory copy, a supporting source example, and a documentation link. Commands are grouped in setup. The separate verification outcomes strip is omitted; the replay's fuller description remains available to assistive technology.

The stage retains its matte materials, ambient animation language, and single pinned canvas. Its scene geometry now follows one configured workspace and proposed change. The workflow heading and following narrative have no forced line breaks or character-width limits; text fills the available column up to the animation gutter.

The original chapter spacing is retained. Each workflow stage now has two short descriptive paragraphs: one explains the capability and one connects it to the agent’s next decision. Verification explains the Billing/Orders retry-flag example and how to compare its two views. Copy remains scoped to the modeled system, declared scenarios, and checked inputs. The hero stays concise, and paragraph gaps use the same reading scale.

Each chapter pairs its heading and description with a copyable source example in the left narrative: workspace configuration, a service link, a behavior replay, and verification assertions. The requirements chapter composes web, orders, and billing under `level: strict`. Later examples are labeled excerpts from `orders/mod.lm`, following `docs/tutorials/4-behaviors.md`. The marked “Give your agent more than code” introduction is removed, so the narrative proceeds directly to the requirements chapter. Its syntax follows the workspace declarations in `docs/language.md` and the behavior tutorial. Each copy control uses the exact displayed source and reports configuration- or code-specific feedback. The specimen uses the existing reading hierarchy and restrained colors.

## Motion and behavior

The story has five scroll-selected chapters: the agent loop, declared requirements, cross-service/repository impact, behavior simulations, and verification. The same right-hand canvas stays anchored throughout. Scene weights ease between the existing meshes, including backward scrolling and interrupted transitions. Selection follows each chapter’s heading rather than the center of a text block, so adding paragraphs does not move the handoff. The “Make intent checkable” heading owns the first requirements scene. A 20px deadband prevents rapid changes near a heading. Outgoing SVG text disappears before incoming text appears; geometry keeps its continuous blend. Geometry buffers are prepared once for one WebGL context; the renderer draws one mesh at rest and overlapping meshes while transitioning.

The example is shared by the supporting source snippets and diagram data through `assets/change-story.json`. Source code stays in the left narrative, beneath each heading and its introduction. The animation contains abstract stacked geometry, named repositories, and plain-language labels; it has no code blocks, diffs, configuration lines, or source filenames. The proposed Orders change preserves Billing’s retry flag throughout the subsequent stages.

Impact traces carry that change from Orders to its modeled caller/provider connections. Simulation fans into the two declared retry cases. Verification shows the same true/false outcomes against the requirement to preserve the retry flag. Before fix breaks the outgoing path and shows missing values; After fix forwards the flag in both cases. The animation is an illustrative declared system: repositories supply the relationships and checker shown.

Each selected phase restarts its four-second change flow on entry, with moving copper markers connecting source, affected code, cases, and evidence. The hero retains its full stage headings, actor labels, supporting captions, central Lemma lockup, and return-path annotations. A 680 × 624 drawing field separates the four stages. Graphics are scaled to 88% and moved farther apart; headings and captions occupy distinct bands above and below them. The three-lane routes cross the open space between stages. Shader motion, materials, phase timing, and the 16-second loop are preserved. Mobile typography scales independently to keep the explanations readable. The verification narrative uses a consistent 24px grid between its heading, description, code specimen, and documentation link. A floating Before/After switch stays at the top left of the pinned animation. Both choices are directly visible; there is no hover interaction, popover, or explanatory status panel. Each button retains a 44px touch target, pressed state, and a nonvisual outcome announcement. The control is available only in the verification scene. Assertions and the illustrative-evidence note remain in the left narrative. Replay controls work while paused or in SVG fallback. Outcomes and setup follow the story rather than interrupting its animation rail.

The stage has a transparent canvas and no panel fill or dotted texture. SVG words and arrowheads stay sharp above the GPU geometry. A normalized fit matches each original SVG aspect ratio to the shared surface. On mobile, the pinned stage uses the page's own paper color to prevent scrolling text from showing through it; the artwork remains transparent.

The renderer caps updates at 30 frames per second and resolution at 1.75 device pixels per CSS pixel, and suspends GPU frames offscreen, while paused, or in a hidden document. Reduced motion swaps scenes directly without blending. Manual pause freezes the current frame; a subsequent scene selection displays the selected static scene. If WebGL is unavailable or its context is lost, the scroll-selected SVG takes over. Context restoration rebuilds buffers for the current selection. Without JavaScript, the overview SVG and complete narrative remain readable. No third-party animation or 3D runtime is loaded. Resource handling follows the [WebGL guidance](https://developer.mozilla.org/en-US/docs/Web/API/WebGL_API/WebGL_best_practices).

The verification outcomes strip is currently omitted. The behavior replay flows directly into setup and retains its Satisfied/Violated result. Before fix drops Billing's retry flag; After fix forwards it in both declared replay cases. Engine benchmarks remain linked in the footer.

The top bar is replaced by a floating icon dock on the left, outside the content grid. Its 18px icons retain 44px targets. Hover and keyboard focus reveal tooltips; Escape dismisses them. Active page sections are marked in the dock. At 700px and below it becomes a floating bottom dock, with GitHub still accessible from the footer.

The dock's Pause motion control stops ongoing animations and exposes all revealed content. The operating system's reduced-motion preference disables animation and smooth scrolling, with static, readable content. The page remains readable without JavaScript. Copy actions preserve exact selectable commands and announce success or manual-copy instructions.

## Claim boundaries

The page describes the current repository implementation, grounded in `docs/language.md`, `docs/verify.md`, `docs/tutorials/2-requirements.md`, `docs/tutorials/4-behaviors.md`, spec 13, and `crates/lemma-graph/tests/behaviors.rs`.

- Requirement checks evaluate declared requirements over modeled relationships and facts. Do not call them formal verification: they do not prove arbitrary program correctness or complete inferred coverage.
- Impact analysis follows the modeled/extracted system and retains findings' supporting evidence.
- Behavior simulations are declared cases executed through user-provided checker commands, not automatic inference of all possible production behavior.
- Satisfied, violated, unknown, and stale remain distinct. Evidence applies to its checked inputs and revision.

The retry example is explicitly illustrative and follows the checker-backed replay tutorial. The page makes no universal wall-clock speedup claim. Repository configuration, requirements, and checkers must be supplied; Lemma remains in pre-release development. The older rc0 checklist predates the implemented `.lm` behavior system, so the language reference and implementation are the source for those capabilities.

## Verification

Checked with local Chrome, Playwright, and axe-core:

- No whole-page overflow across 19 widths from 320px to 1600px.
- No axe-core WCAG A/AA violations at 1440px and 390px.
- SVG motion changes over time and stops when paused.
- Scroll-driven workflow selection updates the diagram and caption.
- Dock hover/focus labels, Escape dismissal, active-section navigation, command-copy values, and before/after states work.
- Reduced motion disables animations and smooth scrolling.
- No failed resources or page errors in the tested flows.
- Desktop and mobile compositions were visually inspected.
- The pinned illustration fits at 1366 × 768 and 1024 × 720; core content remains readable without JavaScript.

After replacing the top bar, the dock and content were checked at 13 widths from 320px to 1440px, with no overflow or dock placement outside the viewport. Desktop/mobile accessibility audits, motion controls, and the reduced-motion state were rechecked.

After the broader positioning revision, fifteen viewport widths from 320px to 1600px and desktop/mobile accessibility audits passed. Model/Impact/Simulate scene selection, before/after behavior outcomes, both replay outputs, copy commands, motion preferences, and the compact outcome strip were verified. The page anchors and SVG XML are checked during packaging.

The four-stage hero loop was checked at each animation phase: the corresponding stage accent and directional packet activate in sequence, including the evidence return to the agent. Pause/resume and reduced-motion rendering passed. The diagram gets a full row at 960px and below to preserve label readability. Fifteen viewport widths, desktop/mobile accessibility audits, existing interactions, SVG XML, and fragment targets passed after this change.

The WebGL revision was checked using the actual GPU output: pixels change while running and remain fixed when paused; the four stages advance in order; frames stop offscreen and with reduced motion; forced context loss switches to SVG and restoration rebuilds the render. Canvas/SVG alignment and the resolution cap passed at seventeen widths from 320px to 1600px. Desktop/mobile accessibility, existing page interactions, WebGL-disabled rendering, and JavaScript-disabled SVG rendering passed.

After extending the material to workflow and verification, all three GPU renderers passed transparent-background, animated-pixel, pause/resume, offscreen/reduced-motion, and context-recovery checks. The three workflow scenes follow scroll selection, including while paused. Before/after replay changes both the GPU paths and the accessible outcomes, including after context restoration. Centered SVG/canvas alignment passed at seventeen widths, including the sticky graphic's letterboxing. Desktop/mobile accessibility audits, WebGL-disabled and JavaScript-disabled fallbacks, unique IDs, SVG XML, and fragment targets passed. Desktop and mobile diagrams were visually inspected.

The hierarchy pass passed the same seventeen-width layout and GPU interaction checks, desktop/mobile accessibility audits, context recovery, and fallback checks. Checksums confirm the WebGL module and all three SVG sources are unchanged; the drawing and animation CSS blocks are preserved verbatim. Removed-caption checks, accessible label targets, unique IDs, fragment links, and SVG XML passed. Desktop and mobile layouts were visually inspected.

The continuous-stage revision passed forward and backward selection across all five chapters using one persistent canvas and an anchored right column. Intermediate scene weights, pause during a blend, reduced-motion swaps, live replay controls, context recovery, and offscreen suspension passed. Eighteen widths from 320px to 1600px passed overflow, canvas fit, resolution-cap, and right-column/mobile-top placement checks. The full-width narrative has no forced breaks below the hero. Desktop/mobile accessibility audits, command copy, dock navigation, deep-link selection, SVG-only and JavaScript-disabled fallbacks, unique IDs, ARIA targets, fragment links, and SVG XML passed. The existing mesh builders are unchanged. Desktop and mobile compositions were visually inspected.

After restoring the original chapter spacing and adding descriptions, all eighteen responsive widths, pinned placement, forward/backward transitions, replay controls, reduced motion, context recovery, desktop/mobile accessibility, copy/navigation, deep links, and SVG/no-JavaScript fallbacks passed. The stylesheet retains the pre-reduction layout rules with only paragraph spacing added. The animation module, scroll controller, and three SVGs remain unchanged. Desktop and mobile copy layouts were visually inspected.

The first-handoff fix passed heading-boundary deadband checks in both directions and sampled the transition to confirm that outgoing and incoming text are never visible together while geometry continues blending. The workspace example parses with the current Lemma language parser, and copying it preserves the exact displayed source. The full eighteen-width story, pinning, replay, accessibility, context-recovery, reduced-motion, and fallback checks passed after the change.

The configured-change revision was subsequently corrected to keep code outside the animation. The source examples support the left narrative while the right-hand illustration shows the proposed change, impact, two declared behavior cases, and verification outcomes.

Automated audits cover tested states, rather than a complete accessibility certification. No Rust sources or runtime dependencies changed.

The supporting-snippets correction passed exact-copy checks for all four narrative examples, code-free illustration labels, heading-to-example spacing, distinct GPU replay outcomes, and mobile layout. The full eighteen-width story, clean scene handoffs, pinned canvas, replay controls, context recovery, reduced motion, desktop/mobile accessibility, and SVG/no-JS fallback checks passed.

The header uses the existing relationship mark rather than the text wordmark. Setup commands use a muted background and a single line with an ellipsis; the full command remains in the DOM, its tooltip, and the copy action. The final refinement passed desktop/mobile accessibility, eighteen responsive widths, scene handoffs, replay states, full command copy, and SVG/no-JS fallback checks.

The hero sizing revision restores the explanatory copy and changes only its diagram layout. Responsive checks confirmed no label collisions or overflow, live GPU motion, a matching SVG fallback, clean scene transitions, replay states, and accessible desktop/mobile layouts. The other approved page refinements remain in place.

The floating switch passed responsive control bounds, direct mouse/keyboard/touch selection, evidence-state updates, and desktop/mobile accessibility. The earlier pinned-placement, scene-handoff, context-recovery, and SVG-fallback checks remain applicable to the unchanged renderer. The hero and all animation source files remain unchanged by this layout revision.

The replay control is simplified to a direct floating switch. Its selected state still updates the illustration and evidence values while preserving all animation sources.

Desktop page padding reserves 84px on both sides to balance the floating left dock. At the 700px bottom-dock breakpoint, both side offsets are removed.

The setup heading and paragraph use the full available column, with no forced line break or character-width cap. Smaller viewports wrap naturally.

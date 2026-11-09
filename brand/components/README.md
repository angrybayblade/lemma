# Lemma components

Framework-independent HTML and CSS for marketing, documentation, and future interfaces. `components.js` optionally supplies static-page tabs and command copying. The guide demonstrates each component in both themes; it does not simulate an operational Lemma web application.

## Install

```html
<link rel="stylesheet" href="/brand/tokens.css">
<link rel="stylesheet" href="/brand/components/components.css">
<script src="/brand/components/components.js" defer></script>

<main class="lm-surface" data-lm-theme="light">
  <!-- Your components -->
</main>
```

The surface sets font, foreground, background, and border-box sizing for its descendants. It does not reset your whole document. Preserve the font paths relative to `tokens.css`. Semantic variables inherit, so a nested `data-lm-theme="dark"` surface can sit inside a light page. Explicitly set the outer theme on `html` if browser-native controls should follow it.

The optional script enhances markup present when it executes. In React, Vue, or dynamically rendered content, implement equivalent state and keyboard behavior in the framework. Do not attach both implementations to the same tabs. No JavaScript is required for the basic visual styles, links, labels, or outcome badges.

## Type and text

- `.lm-display`: responsive editorial headline, Instrument Serif.
- `.lm-heading`: responsive section heading, Instrument Serif.
- `.lm-eyebrow`: short supplemental metadata; uppercase Geist Mono. Avoid it for essential instructions.
- `.lm-code`: inline monospaced text.
- `.lm-link`: underlined text link.
- `.lm-sr-only`: accessible content without a visible layout box.

```html
<p class="lm-eyebrow">For people + coding agents</p>
<h1 class="lm-display">Move fast.<br>See what breaks.</h1>
<p>Catch breaking changes between services before code merges.</p>
```

Choose heading levels for the page's structure, not the visual size. Do not put large blocks of prose in `.lm-eyebrow` or display serif.

## Actions

Use one primary action per group. Use an anchor for navigation and a button for a local action. Minimum button height is 44px; controls retain a visible focus ring.

```html
<a class="lm-button" href="/docs/quickstart">Get started</a>
<a class="lm-button lm-button--secondary" href="/docs">Read the docs</a>
<button class="lm-button lm-button--quiet" type="button">Dismiss</button>
<button class="lm-button" type="button" disabled>Unavailable</button>
```

| State | Contract |
| --- | --- |
| Default | Clear verb, comfortable target, one primary action |
| Hover | Color or surface changes; layout stays still |
| Focus | 2px semantic focus outline, offset 4px |
| Disabled | Native `disabled` on a button; explain the unavailable action nearby when needed |
| Loading | App sets `aria-busy="true"`; retain a stable width and a meaningful loading label |

Do not add `disabled` to an anchor: HTML does not disable it. Render unavailable navigation as non-interactive text with an explanation, or remove its destination and interaction in your application.

## Outcomes

```html
<span class="lm-badge lm-badge--satisfied">✓ Satisfied</span>
<span class="lm-badge lm-badge--violated">× Violated</span>
<span class="lm-badge lm-badge--unknown">? Unknown</span>
<span class="lm-badge lm-badge--stale">◷ Stale</span>
```

Badges are display text, not clickable controls. Labels carry the meaning independently of color. Announce newly evaluated outcomes through an appropriate enclosing status region; do not make every badge a live region. Keep the exact outcome returned by the underlying evaluation.

## Command terminal

Commands remain selectable text. The terminal deliberately uses the same dark palette in both themes. Add `data-lm-copy` for optional clipboard behavior; its value is the exact command to copy, without the shell prompt.

```html
<div class="lm-terminal">
  <div class="lm-terminal__bar">
    <span>terminal / example invocation</span>
    <button class="lm-terminal__copy" type="button"
      data-lm-copy="lemma verify change.yaml"
      aria-label="Copy lemma verify command">Copy</button>
  </div>
  <pre><code><span class="lm-terminal__prompt">$</span> lemma verify change.yaml</code></pre>
</div>
```

The copy script adds an accessible status message. To use your own visible status region, give it `role="status"` and its own unique ID; point the button's `aria-controls` at it. When the clipboard API is unavailable, the message supplies the value for manual copying. The command always remains selectable in the terminal. Long commands scroll inside the terminal rather than widening the page.

Do not fabricate a CLI transcript to sell the product. Mark illustrative output as illustrative, or capture real output with its input and version.

## Fields

Use an explicitly associated label and helper text. Errors must identify the problem in words. Validation belongs to the consuming application; the brand guide's example is only a label-format demonstration.

```html
<label class="lm-field" for="repository">
  Repository label
  <input class="lm-input" id="repository" name="repository"
    type="text" aria-describedby="repository-help repository-error">
</label>
<p class="lm-help" id="repository-help">A short label for this repository.</p>
<p class="lm-help lm-help--error" id="repository-error" role="alert" hidden></p>
```

On an invalid submission, set `aria-invalid="true"`, reveal the error text, and focus the field if that is the next useful action. Clear stale feedback when the value changes. Do not validate aggressively on every keystroke or rely on a red border alone. Native input text is at least 16px to avoid mobile focus zoom.

## Finding

A finding shows the outcome, a concrete statement, the affected relationship, and source context. The example follows the quickstart; it is not a captured Lemma result.

```html
<article class="lm-finding">
  <div class="lm-finding__header">
    <span class="lm-badge lm-badge--violated">× Violated</span>
    <h3>A caller reads a removed response field.</h3>
  </div>
  <p>Billing removes <code>receipt_url</code>. Web still reads it.</p>
  <div class="lm-finding__location">billing / createCharge → web / checkout</div>
</article>
```

For a real integration, link actual source locations and include the revisions and evaluation scope. Do not invent paths. Show empty, unavailable, unknown, and stale states explicitly. A useful empty state is “No findings in the evaluated scope,” followed by the scope; “Everything is safe” overstates the result.

## Callouts and feature summaries

`.lm-callout` provides one short piece of context. Use a heading only if the callout needs it; keep the text actionable. `.lm-feature` is a simple rule, title, and explanation; arrange features with your own layout rather than nesting decorative cards.

```html
<aside class="lm-callout"><p>Re-index the changed files before checking this symbol.</p></aside>
<article class="lm-feature">
  <span class="lm-eyebrow">01 / Connect</span>
  <h3>See the connections.</h3>
  <p>Read the repositories involved in a change.</p>
</article>
```

## Tabs

Use tabs for alternate views of the same subject, with unique tab and panel IDs. The optional component script implements automatic activation with Left/Right, Home, End, click, and roving keyboard focus.

```html
<div class="lm-tabs" data-lm-tabs role="tablist" aria-label="Finding details">
  <button class="lm-tab" id="summary-tab" role="tab" type="button"
    aria-controls="summary-panel" aria-selected="true" tabindex="0">Finding</button>
  <button class="lm-tab" id="details-tab" role="tab" type="button"
    aria-controls="details-panel" aria-selected="false" tabindex="-1">Context</button>
</div>
<div id="summary-panel" role="tabpanel" aria-labelledby="summary-tab">Finding content</div>
<div id="details-panel" role="tabpanel" aria-labelledby="details-tab" hidden>Context content</div>
```

Inactive panels must be `hidden` so their controls leave the tab order. These tabs use horizontal orientation. For remote or slow panel loading, use manual activation instead of copying this automatic-activation behavior. Link-based section navigation should remain ordinary anchors.

## Tables

Use `.lm-table` on semantic `<table>` markup. Include a caption when the table's subject is not established by its heading. Use proper `th` and `scope`. Wrap dense tables in a locally scrollable container when they cannot reflow; preserve whole-page fit.

## Theme and accessibility responsibilities

Use semantic tokens rather than primitive hex colors inside components. Honor the provided focus styles, native keyboard behavior, and reduced-motion tokens. Text and meaningful control boundaries use contrast-safe tokens; decorative separator borders intentionally use a subtler color.

The consuming application remains responsible for correct names, heading structure, focus management, useful feedback, routing, and real data. The brand guide is checked at 320–1440px, with light/dark previews and automated accessibility checks; that does not certify future applications built with these styles.

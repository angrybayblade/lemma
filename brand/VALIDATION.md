# Brand kit validation

Checked 2026-11-07 in a separate headless Google Chrome session, using Playwright and axe-core. No application or Rust code changed.

- The guide rendered without page errors or failed resource requests; all three local font families loaded.
- Layout checks found no whole-page horizontal overflow at widths 320, 375, 390, 600, 601, 768, 850, 1024, 1100, 1280, and 1440px.
- axe-core checks for WCAG A/AA rules reported no violations in light desktop, dark desktop, and light mobile views after theme transitions settled. This is an automated check, not a full accessibility certification.
- Tab keyboard navigation, Home, focus movement, panel visibility, invalid/valid field feedback, exact clipboard values, and theme switching worked.
- All download and documentation links in the guide resolved locally. Relative Markdown documentation links point to existing files.
- Reduced-motion mode sets transition durations to zero and disables smooth scrolling.
- All approved semantic text pairs meet at least 4.5:1 contrast; the lowest checked pair is 4.56:1. Light violated text on its soft surface is 5.54:1.
- All 21 SVGs parse, have a viewBox and accessible title, and contain outlined typography rather than external font dependencies.
- All six campaign PNGs match the dimensions of their SVG masters. The kit also includes avatar, brand-board, and favicon exports.
- The main guide, component examples, and campaign artwork were visually inspected. JavaScript syntax and Git whitespace checks passed.

The supplied guide-desktop PNG is a preview. Vector campaign masters and token source files remain the editable sources of truth.

# Rendered console review — 2026-09-18

Source: authenticated console at `http://localhost:3020`, inspected with
`agent-browser` at the user's request. Applied `extract-design-system`'s
observe/review workflow through the browser rather than its public-site CLI;
no generated token file replaced the existing source of truth. Applied
`design-system-patterns` to theming, component composition, and interaction states.

## Observed foundation

Samples: `/projects`, `/logs`, `/settings`, plus the login screen. Both theme
classes were inspected at a 1200px viewport. Theme classes were changed locally
for measurement and restored; no settings or records were submitted. Dynamic
content can arrive after an initial sample, so control counts are not an inventory.

- Page titles: Geist, 24px, weight 600, line-height 32px.
- Standard form controls: 14px type, 40px height, 6px corner radius.
- Compact controls: 24–36px heights in dense toolbars and pagination. Preserve
  intentional density; do not normalize all controls to one size.
- Base radius: 0.5rem; semantic background, foreground, primary, muted, and border
  properties change between themes. Success, warning, and destructive retain
  the existing status palette. No replacement palette was introduced.
- No document-width overflow in these desktop samples. This is not a claim
  that every production page has passed a mobile or accessibility audit.

## Changes

| Before | After | Why |
| --- | --- | --- |
| Sandbox had no theme selection | Light/dark/system preview using the console provider | Review all examples against real theme tokens |
| Gallery nested a second h1 | Nested PageHeader explicitly uses h2 | Preserve one page title |
| Busy button ARIA could be overwritten by caller props | Busy semantics take precedence; aria-disabled clicks are guarded | Keep focus and prevent duplicate actions |
| Inactive settings save could submit using Enter | Form guards clean, invalid, and saving states | Keyboard and pointer behavior agree |
| Example still appeared dirty after saving | Saved values become the baseline; status explains changes | Demonstrate a complete, honest save cycle |
| Gallery used raw emerald text classes | Existing success token | Keep status examples aligned with both themes |

## Verification

- Regression tests cover busy ARIA semantics and form submission in clean,
  invalid, saving, and valid states.
- Browser checks on the sandbox verified a dark theme save cycle, persistence
  across routes, one gallery h1, and no page overflow on settings at 390px.
- Console typecheck, package lint, and sandbox production build validate integration.
- No visual-baseline suite added. These are sampled checks, not exhaustive
  contrast or assistive-technology certification.

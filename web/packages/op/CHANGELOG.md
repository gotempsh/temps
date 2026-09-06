# Changelog

## 0.1.2

- The skin class is now `operator ink v1`, the first published version of the
  system. The unreleased `.v4` and `.v5` classes are gone; their rules are
  consolidated unchanged into `.operator.ink.v1`, so a root that used to carry
  `operator ink v4 v5` carries `operator ink v1` and renders identically.

- `GeoMap` reads the hovered country at the pointer on a fine pointer and no
  longer renders a readout row under the map on desktop. Below md the row
  under the map stays and becomes the touch reader: tap a country to read it,
  tap it again to open. One visually hidden live region announces the readout
  in both cases.

## 0.1.1

Accessibility, from the sandbox's first axe run (design-system/e2e/a11y.spec.ts).

- `Ledger` sortable headers no longer set `aria-sort` on a `<button>` (only valid
  on a column header inside a row, which a CSS grid is not); the sort state is
  spoken as part of the button's name ("issue, sorted ascending").
- `Picker` always has an accessible name: new `label` prop (what the field is),
  falling back to the placeholder. A `role="combobox"` takes no name from its
  contents, so the visible value never counted.
- `PageState` loading skeleton is a `role="status"` region (aria-label was
  prohibited on a role-less div).

## 0.1.0

Initial release. Extracted from the `design-system/` sandbox
(`src/components/op/*`) into a real package so the console and the sandbox
render the same components instead of the sandbox owning a private copy.

- Moved every op primitive out of `design-system/src/components/op/` and
  rewrote its `@/` alias imports to relative paths — the package has no
  path-alias dependency on any host app.
- Vendored the minimal shadcn-style primitives the op layer needs into
  `src/ui/` (alert-dialog, button, command, dialog, copy-button, input,
  popover, skeleton, tooltip) plus `src/lib/cn.ts` and `src/lib/clipboard.ts`,
  so the package is self-contained and skinnable.
- Extracted the operator token layer and every `.op-*` rule from
  `design-system/src/globals.css` into `src/op.css`, prefixed with
  `@source "./"` so consumers get the package's utilities generated for free.
- `design-system/src/components/op/index.ts` is now a re-export of this
  package; the sandbox consumes it through a Vite alias.

# Changelog

## 0.1.2

- Kind icons have a slot of their own, everywhere a list mixes kinds (brand
  guidelines §6, "an icon wherever it adds context").

  - `LedgerRow` takes `icon`: what kind of record the row is (app / worker /
    static project, database engine, control plane / worker node, span kind).
    It renders in a fixed 16px slot at the head of the first cell, and before
    the name on a phone, in muted ink. It rides the first cell rather than
    taking a grid track of its own, so no caller's `grid` string changes and no
    single-kind ledger carries an empty slot. Row heights are unchanged.

  - `PickerOption.icon` no longer shares the state glyph's slot. The glyph slot
    keeps the state (and the ● that marks the current value); the icon gets its
    own 16px slot after it, in muted ink, and is never tinted by `state`. An
    option with an icon is therefore marked selected by the same ● as every
    other option, so the trailing `Check` is gone. Callers that passed both
    `icon` and `state` (the permission-mode picker) now read as a glyph *and* a
    mark rather than a coloured mark.

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

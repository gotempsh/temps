# Changelog

## 0.1.2

- `TimeChart` tells series apart by pattern, not by hue, and draws its own
  legend (audit item 31; see `design-system/docs/data-viz.md`).

  - `Series` takes `stroke` (`'solid' | 'dashed' | 'dotted'`) and `weight`
    (`'thin' | 'regular'`), defaulted by position (solid regular, then dashed,
    dotted, solid, each thin). `stroke` used to be a CSS colour and is now the
    dash pattern; `width` still takes an exact pixel width and still wins.
    `--chart-1` / `--chart-2` are gone from the component: every line is ink,
    and a line takes a tone only when `series.state` says the series *is* a
    state (an error rate read against its threshold band).

  - The legend is generated from `series`: the swatch is a sample of the real
    line (same dash, same weight, same ink), the name is muted, and the value
    at the cursor rides the label. A hand-written legend in a `ChartFooter`
    (`thick p50, thin p99`, `the thin line is users`) is now always wrong — it
    cannot be matched to a line and it drifts. `legend` defaults to on with
    more than one series. More than four series logs a dev warning.

  - `table` (default on) puts a "table" toggle beside the legend that swaps
    the plot for the same buckets as an `.op-rows` table — bucket · value per
    series, deploy markers in the bucket cell, same height region, no
    animation — so every chart can be read as numbers.

  - The plot is `role="img"` with an `aria-label` sentence built from the new
    `title`, `range` and `verdict` props, falling back to the series names and
    the axis bounds, so a chart is never an unlabelled graphic.

- `Field` carries the whole anatomy: `label` (always visible), `hint` (`help`
  is kept as the older name for the same line), `error`, and `optional`. The
  error renders under the hint as glyph + sentence in the destructive tone —
  the one place a field carries colour — and the hint stays put while it
  shows, because advice and fault are different things. Pass `id`, or use the
  new render-prop form (`{(c) => <Input {...c} />}` with
  `FieldControl = { id, aria-describedby, aria-invalid }`), and the control is
  wired: the hint and the error are described-by, never part of the control's
  accessible name, and the label switches from wrapping to `htmlFor`. A field
  with neither hint nor error renders exactly as before, at the same height.

- New `FormErrors`: the summary a form shows when more than one field fails on
  submit. One error `Callout` at the top of the form, each entry a button that
  focuses the field it names (`{ id, label, message }[]`, `min` failures
  before it appears, default 2). The inline message under each field stays
  where it is; the summary is a way in, not a second copy of the truth. See
  `design-system/docs/forms.md`.

- New `fmt` module: `fmtNum`, `fmtPct`, `fmtBytes`, `fmtDuration`,
  `fmtRelative`, `fmtAbsolute`, `fmtCount` and `EMPTY`. Pure functions that
  hold the number, date and duration rules of
  `design-system/docs/content.md` in one place — locale grouping through
  `Intl`, decimal bytes (binary on request), percentages at one decimal,
  durations in at most two units, time relative under 24 hours and absolute
  after, plurals through `Intl.PluralRules`, nothing as an en dash and zero
  as `0`. `Num`, `Pager`, `Breakdown`, `Funnel`, `Flow`, `Histogram` and
  `TimeChart`/`RangePicker` now format through them instead of ad-hoc
  `toLocaleString` / `toFixed`; rendered output is unchanged.

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

- The token layer is data. `tokens.json` (W3C DTCG, exported as
  `@temps-sdk/op/tokens.json`) carries two layers: `base` — the paper/ink pair,
  the five state hues, the faces, radius, border, the 4/8/12/16/20/24/32 scale,
  the six type tiers, and motion — and `semantic`, which is exactly the custom
  properties `.operator.ink` declares, light and dark, aliased to base with
  `{base.x.y}`. `scripts/tokens.mjs check` (wired into the design system's
  `bun run lint`, and `bun run tokens:check` here) parses both files and fails
  with a diff on any value, any name present on one side only, and any
  ordering difference. `scripts/tokens.mjs build` prints the block it would
  generate; op.css is still hand-written and still the source of truth, so
  generation is the next step and this release only enforces the mirror.

- Motion has tokens: `--op-duration-fast` (80ms), `--op-duration` (100ms, the
  frozen default), `--op-duration-slow` (200ms) and `--op-ease`
  (`cubic-bezier(0.2, 0, 0, 1)`), plus `.op-motion` / `.op-motion-fast` /
  `.op-motion-slow` to opt one element into a different tier or into
  `border-color` / `opacity`. The three carry a `:not(.animate-spin)` so they
  reach the blanket rule's specificity — without it the blanket `!important`
  swallowed them and the classes did nothing. Every literal duration in the package is gone:
  the blanket transition rule, the switch track and thumb, and the dialog and
  alert-dialog surfaces (`duration-200` →
  `[transition-duration:var(--op-duration-slow)]`) all read the tokens.
  Resolved values are unchanged. `@media (prefers-reduced-motion: reduce)`
  zeroes all three durations and forces transition, animation and iteration
  count across the skin in one rule — previously the skin had none.
  `--op-raise-shadow` replaces the literal in `.op-raise`. New docs:
  `design-system/docs/motion.md` (what may move, what never moves, and the
  exceptions that exist today) and `design-system/docs/icons.md` (lucide,
  stroke 1.75, 16px in rows and 14px in labels, and the concept → icon table
  that stops two screens using two icons for one thing).

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

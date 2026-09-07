# Changelog

## Unreleased

- **`useUrlState` and the rest of the URL-state hooks moved into the package**
  (`src/url-state.ts`, exported from the index): `useUrlState`,
  `useUrlNumber`, `useUrlPatch`, `useUrlWindow`, `useUrlSort`, `useUrlText`,
  `forNewView`, `VIEW_KEYS`, `KEPT_ON_NAVIGATION` and their types. They were
  the design-system sandbox's, which meant every other consumer had to
  re-derive "the URL is the view" by hand. `forNewView(params, keep?)` now
  takes the keep-list as a parameter — the `p` / `fresh` / `fail` default is a
  routing convention, not a rule. `react-router` (bare, v8) is a new peer
  dependency: the hooks sit on its `useSearchParams`.
- **The package is publishable.** `bun run build` emits bundler-targeted ESM
  plus `.d.ts` and maps to `dist/` (git-ignored) via `tsconfig.build.json`;
  `main` / `module` / `types` / `exports` point at it, with a `"source"`
  condition for bundlers that prefer the TSX and the sandbox's Vite alias
  untouched. Everything the source imports is now a dependency or a peer
  dependency instead of resolving through the monorepo root. `README.md` is
  the consumer setup, including the `@source` line a consumer's Tailwind needs
  in order to generate the utilities the primitives render.

## 0.1.2

- `.op-prose` is the whole look of a body of long-form prose, not just "the
  sans face": measure (`--op-measure`, ~68ch, capped inside the frame and
  never by narrowing it), headings on the type ladder (h2 carries a rule, h5/h6
  are just bold), `.op-lede`, ink links with an underline offset and never
  blue, square ink bullets and tabular ordinals, GFM task lists, a blockquote
  as an ink rule on the left (not italic, not grey), `hr`, inline `code` at
  0.875em on `--muted`, `pre` as a square inset pane at mono 12px scrolling
  sideways, tables in the ledger idiom (`op-label` header, 1px rules, numbers
  right through `data-align="end"` or `.num`), framed square figures with
  `fig. N · …` captions from a CSS counter, `kbd`, `details/summary`,
  footnotes, and `mark` as an ink underline rather than a yellow wash. Putting
  the class on a single element still means only "this wraps": every rule is a
  descendant rule. `.op-raw` is the escape hatch for a live component dropped
  into a document. New token: `--op-measure`.
- `Article`, the fourth page template: a page that is read top to bottom.
  Title, lede, byline (mark · name · absolute date · reading time), a right
  rail built from the body's own rendered h2/h3 (sticky, current section in
  ink, every entry a real link), the body in `.op-prose`, and a "back to all
  posts" footer. `CodeBlock` is a fence with a language/filename label row and
  a `CopyAction`. `ImageFigure` is a framed, captioned picture that takes
  `src` and `dark` and swaps them with the theme, and always renders both an
  alt and a caption.
- `fmtAbsolute` takes `time: false`, which drops the clock and prints the year
  (`Sep 1, 2026`). A published date is a day, and `00:00` beside it is a time
  nobody measured.

- `SessionTimeline` axis draws ticks instead of glyphs (page view a full rule,
  other events short, errors red), on a track inset from the frame so `0s` is
  inside it; the range scrubber spans exactly the track so the thumb sits
  under the cursor line. Glyphs stay in the list.
- `CopyAction` and `useCopy`: a copy answers on the control that was pressed
  (`✓ copied` for two seconds, `× couldn't copy` in red with the reason), never
  in a toast. The idle label holds the width under the answer. `Inspector` takes
  `copyLink` (the row's address) and copies it itself; `onCopyLink` is gone,
  `onCopied` fires after a successful copy.
- Scrollbars belong to the skin: thin, square, ink at 30% on a transparent
  track (55% under the pointer), the document scrollbar included when the
  skin owns the page; sideways strips (`.op-scroll-x`) show no bar.
- A hover, a selection or a focused row lifts the whole row: `--muted-foreground`
  goes to 80% ink under the fill, and to 78% paper under an ink-filled selection
  (the palette's current item, a filled tab). Utilities read the variable, so
  text, state glyphs and lucide icons all step up together and the right-hand
  side of a row is never dimmer than the rest while the reader is on it.
- `Button` takes `busy` and `busyLabel`: while the work runs it spins its own
  icon (`.op-busy`, `0.9s linear` — the one spin in the system), says the verb
  in progress, and keeps its width, its colour and its focus. It is **not**
  `disabled`, because disabling drops focus at exactly the moment the reader is
  waiting to hear what happened. `Settings` takes `saving` and passes it to the
  sticky save bar. Both labels share one grid cell, so the width never moves.
- Switching a tab never scrolls the document; the strip reveals the active tab
  sideways only (`revealInRow`, replacing a `scrollIntoView` that also moved
  the page vertically).
- `Ledger` arrow keys no longer act while another widget has focus. `j`/`k`
  stay page-wide accelerators; `ArrowUp`/`ArrowDown` act only when the ledger
  has focus or nothing does, so one arrow on a heatmap no longer moves the
  cursor in every ledger on the page.
- `Inspector`: a right-hand panel that inspects a ledger row beside the list.
- `KbdPair` renders two keys that are one idea as one badge — `j / k · down / up`.
- Density washes (`inkCell`) paint in `--op-ink-wash`, which is black on both
  token layers, so "more" is always darker than the ground. Painting them in
  `--foreground` put a 50% grey in the middle of the ladder that no text colour
  could sit on, which axe caught on night.

- A sixth `State`, `running` (`◉`), for work happening now: building,
  restoring, scanning. It is ink, not a hue — `GLYPH_CLASS.running` is
  `text-foreground` — because running is not a verdict; the word comes from the
  operation and the pulse is what says "now". `STATE_RANK` places it between
  `warn` and `sampled`, so "needs attention first" sorts error 0, warn 1,
  running 2, sampled 3, ok 4, idle 5. **`State` is now six values, so every exhaustive
  `Record<State, …>` needs a sixth arm** — tone maps take the neutral/ink value
  they already use for a non-tone, never a hue.
- `.op-pulse`: an opacity-only animation (`1.6s ease-in-out infinite
  alternate`, 1 → 0.45), the one sanctioned exception to "the system does not
  animate". `glyphClass(state)` (exported from `status.tsx`) applies it to a
  `running` glyph and is now used at every glyph site in `status.tsx` and in
  `Stages`. It is lifted out of the blanket `.operator`, `.operator.hardline`
  and `.operator.ink` motion rules by `:not(.op-pulse)`, and zeroed under
  `prefers-reduced-motion: reduce` (`animation: none; opacity: 1`). See
  `design-system/docs/motion.md`.
- `PageState`'s retry button no longer spins while retrying: its `RefreshCw`
  takes `.op-pulse`. `animate-spin` is now sanctioned only inline on a button
  that is submitting.
- `Stages` reads `state: 'running'` as the step in flight (the older
  `idle` + `lines` shape still works), so the running step's glyph pulses and
  its log streams.
- Night is not paper inverted: dark `--border`/`--input` are 62% ink and the
  raise falls in `--border`; `--muted` sits below the ground; `--muted-foreground`
  0.68 → 0.74; state hues at chroma 0.11–0.14. See brand §4 "Night is not paper
  inverted".
- Floating content (tooltip, popover, menu, select) appears in place: the
  skin no longer transitions `transform` on Radix poppers, which used to slide
  every panel in from the top-left corner. The shadcn entrance/exit animation
  classes were removed from the popover and dialog primitives.
- Agent primitives (`agent.tsx`): the blocks an AI is allowed to render, and
  what proves it (see `design-system/docs/generative-ui.md`).

  - `ToolRow` is one typed call as one row — kind icon · state word · title ·
    meta · duration — collapsing to its input, output, diff or error, with the
    inline approval (`Y` / `N`, and a red left rule with `run it` only when the
    action is irreversible). `TOOL_STATE` fixes the eight state words;
    `toolKind` maps a tool name (a coding agent's `run_command`, the console
    assistant's `get_error_time_series`) to its icon.

  - `Provenance` wraps any generated block with the call that produced it —
    `from query_metrics · 41m ago · 7d` — and a `show query` toggle. `tool` and
    `when` are required, so the component cannot render an unsourced chart.

  - `Proposal` is the propose-then-confirm gate for every write: action ·
    target · consequence · reversibility · autonomy level, confirm or decline,
    routed through `EchoDialog` when `irreversible`. Nothing runs until a human
    answers, and the agent never confirms its own proposal.

  - `StreamBlock` holds the shape of the block that is coming (text, chart,
    ledger, detail, keyvalue, tool) at the height it will land at. Static: no
    shimmer, nothing draws itself.

  - `AgentQuestion` (typed options, pick then confirm, `1`–`4` and `⏎`),
    `AgentSources` (what was read, as links into the console's own records) and
    `RunAside` (model · workspace · mode · context · checkpoints as `KeyValue`).

  - `AgentRow`, `AgentInset`, `AgentDiff`, `AgentGlyph` and `AgentKindIcon` are
    the shared pieces the sandbox's `/agent` transcript is built from; they
    moved out of `AgentChat.tsx` unchanged so the console and the sandbox draw
    the same ledger.

- Date, time, range, duration and schedule fields (`datetime.tsx`), so a form
  can ask for a moment without inventing a control (see
  `design-system/docs/forms.md` §"Dates, times and ranges").

  - `DateTimeField`, `DateField`, `TimeField` compose `Field` around a real
    `datetime-local` / `date` / `time` input: typed entry first, the browser's
    picker as the accelerator, ↑/↓ stepping the focused segment. `precision:
    'second'` adds `step=1` for the one second-precise operation (point-in-time
    restore). `zone` is a mono fact beside the control and becomes a `Picker`
    when `onZoneChange` is passed — a control never guesses which clock it is
    on. `min`/`max` state the window in the hint once and fault on blur;
    `presets` fill the absolute field, which stays the truth about what they
    wrote. `never` makes "no expiry" an option word, so an empty date can never
    mean forever.

  - `DateTimeRangeField` is two of them on one row (stacked below sm) with
    `quick` windows, `to > from` validated on blur of "to", and ranges past
    `retentionDays` struck through with the plan word rather than hidden.

  - `DurationField` is a number plus a unit `Picker` (`s` `min` `h` `d`) over a
    millisecond value, so `30d` is never free text to be parsed; the preview
    under it is `fmtDuration`.

  - `ScheduleField` is `HH:MM` plus its zone plus optional weekday toggles, and
    it prints the next three runs underneath — a schedule nobody can read back
    is a cron expression with extra steps. `cron` stays available as the
    advanced entry beside the simple one, never instead of it.

  - `Strip` is the one anchor strip these and `RangePicker` share, so a preset,
    a quick range and a weekday are the same control and the gating (struck
    through, still pressable, calls `onGated`) cannot drift between them.
    `RangePicker` renders through it and is otherwise unchanged, except that
    `custom` now takes a required `zone` and names it under the two inputs.

  - `op.css` gains a block for the native controls under the ink skin: mono,
    tabular, square, muted calendar glyph (inverted in dark), the focused
    segment marked in ink rather than the UA's selection blue.

- `fmtStamp` in `fmt.ts`: a wall clock written ISO-ordered
  (`2026-09-06 20:33`, `… :41` at second precision, `zone` appended). It
  converts nothing — the value is already a wall clock in that zone, and a
  formatter that helpfully shifts it is how a restore lands on the wrong
  second.

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

- Data visualisation, second wave (`viz-ink.tsx`, `viz-time.tsx`,
  `viz-grid.tsx`, `viz-graph.tsx`, `viz-usage.tsx`): the forms the Temps
  console needs that `TimeChart`, `Breakdown`, `Funnel` and `StatusStrip`
  could not draw. Rules in `design-system/docs/data-viz.md` §§9–23; rendered
  in `DataVizBlocks2` (`viz-band` … `viz-topology`).

  - `TimeChart` gains `band` (an expected range as a hatched ink band, never a
    filled area), `anomalies` (× on the line, listed by the caller) and
    `compare` (the prior period as a dotted thin ghost, with the delta and its
    baseline in the generated legend). `BandChart` wraps them for the metrics
    explorer: it derives the excursions from the data and the band, draws the
    out-of-band stretch of the line in the state tone with a × at its peak —
    colour is allowed there because that segment *is* a state — and adds the
    focusable list of anomalies and a "vs expected" column in the table view.

  - `StackedInk` is composition over time as stacked **bars** from zero — at
    most four layers told apart by hatch, dot and solid at three greys, with
    state tone only on the layer that *is* a state. A stacked area stays
    banned. `LatencyHeatmap` is time × latency bucket at five ink steps, with
    percentile overlays that land in the bucket they belong to.

  - `PercentileLadder` (p50 · p95 · p99 · max, each with its own baseline
    delta), `CohortGrid` (retention as a real `<table>`), `DeltaTable`
    (metric · before · after · delta, tone only at a threshold).

  - `PathTree` replaces the banned Sankey for analytics journeys;
    `SessionTimeline` is the timeline-and-list contract around rrweb;
    `Topology` is a deterministic layered graph with the same nodes as a list
    beneath it, the way `GeoMap` sits under a ranked list.

  - `UsageBar` states usage against an allowance in words before the bar, with
    the overage hatched and the sampling point marked. `Gauge` is a
    `MetricGrid`-shaped cpu/memory/disk tile with threshold ticks that carry
    their own words; radial gauges are banned. `StateTimeline` draws a
    resource's real transitions with their durations, where `StatusStrip`
    draws equal buckets for a ledger. `WindowTimeline` puts backups, the
    WAL-covered window and the restore cursor on one axis above the PITR form.

  - Shared: `Figure` (`role="img"` sentence plus the "table" toggle),
    `DataTable`, `InkPatterns`, `useReadout`, `ReadoutLive`, `inkCell`,
    `inkStep` and the `INK_*` tokens, so no figure invents its own greys,
    hatch or keyboard readout. `.op-ink-hatch` / `.op-ink-hatch-error` are the
    CSS half of the same vocabulary for HTML bars.

- `CalendarHeatmap` gets the readout it never had. Its cells used to carry
  only a native `title` — a delayed tooltip that never appears on touch and is
  unreachable by keyboard — and its legend said "less … more" with no numbers.
  It now follows the `GeoMap` rule: the readout is at the cursor on a fine
  pointer, a row under the grid below `md` (tap to read, tap again to open),
  and the grid is one focusable region where `←` `→` move a week, `↑` `↓` move
  a day and `⏎` opens, with one `aria-live` line. The legend prints the
  numbers behind the five swatches. New props: `unit`, `ids` per day (so the
  readout names what shipped) and `onOpen`.

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

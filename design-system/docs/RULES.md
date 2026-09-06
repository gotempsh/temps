# Temps design system: rules for agents

Machine-readable digest of `brand-guidelines.md`, `design-system-handoff.md`,
`content.md`, `localisation.md`, `forms.md`, `notifications.md`, `data-viz.md`,
`motion.md` and `icons.md`.
Imperative only. When this file and those documents disagree, they win — fix this file.
Rendered at `/guide#tooling`. Reference implementation: `/v1`, `/op-components`.

## Setup

- Import `@temps-sdk/op/op.css` before any rule in the stylesheet.
- Put `operator ink v1` on the root element you want skinned.
- Import primitives from `@temps-sdk/op`. Do not edit the package from a consumer.
- Pass the skin class to portalled content (dialogs, toasts, command palettes).
- Restart the dev server after introducing a Tailwind class new to the codebase.

## Non-negotiable

- Use paper and ink only. Background warm off-white, text near-black. Dark inverts the same pair.
- Make every border ink (`--border` equals `--foreground`). Use `--op-rule-soft` only for row dividers.
- Ship no cards. Use one `.op-raise` per screen, on the thing the reader must act on.
- Emit colour only through `Status`: glyph, word, tone, in that order. Never a bare tone.
- Keep whitespace between sections, not inside tables.
- Freeze radius at 0.25rem, borders at 1px, spacing at 4/8/12/16/20/24/32px.
- Use Geist and Geist Mono. No other faces.
- Set numbers in mono, tabular, unit after the value in muted.

## Tokens

- Take every value from a token. A hex, an `oklch()` or a `ms` literal in a tsx file is a bug.
- Name only semantic tokens in a component (`--muted-foreground`), never a base token.
- Edit `web/packages/op/tokens.json` and `op.css` in one commit. `bun run lint` fails when they disagree.
- Add a token to light; add it to dark only when the value actually changes. Dark cascades.
- Keep the scale closed: radius 0.25rem, borders 1px, spacing 4/8/12/16/20/24/32, six type tiers, three durations.

## Banned

- Tailwind palette literals (`text-red-500`), a hex or an `oklch()` in tsx.
- A literal duration in a tsx file. `duration-150`, `transition-all`.
- A second hue. `data-accent` is landing-only, one filled element per viewport.
- Spinners as page state. Use `PageState state="loading"` (skeleton rows).
- Blank empty states. Every non-happy state is `PageState`.
- Confirm dialogs that are not `EchoDialog`.
- `<select>` for branches, images, regions, environments, or >7 options. Use `Picker`.
- Titles at weight 500. Titles 600–800, body 400, labels 500.
- Cards as layout. Stock shadcn card look.
- Sparkles, gradients, wands, the word "magic", `Loader2` as a page. `Sparkles`, `Wand2`,
  `WandSparkles` by name — AI is `Bot` and `Brain`.
- Hiding a feature because it is unconfigured. Show it, say what is missing, link the fix.
- Hand-rolled hovers on filled controls. Use `.op-fill-ink` / `.op-fill-destructive`.
- `href="#"` with `preventDefault`. A `Kbd` badge with no handler. A filter that filters nothing.
- Tabs on a single record. Tabs inside tabs. Two `Ledger`s on one screen.
- Red on a confirmation that is reversible. Red means irreversible loss only.
- A mixed-kind list with no kind icons.
- A kind icon in the state glyph's slot, or a kind icon carrying a state colour.
- Filled icons, emoji, icons as bullets, two icons for one concept.
- A per-icon `strokeWidth` or `size` prop. A brand-coloured logo that is not `GitProviderLogo` or `ProjectMark`.
- A token in `tokens.json` that `op.css` does not declare, or the reverse.
- A skeleton that shimmers, a chart that draws itself, a number that counts up.
- "Something went wrong", "an unexpected error occurred", "please try again later", "contact support".
- `OK` / `Yes` / `No` / `Submit` / `Confirm` as a button label, and `Are you sure?` as a dialog title.
- `toFixed`, `toLocaleString`, a hand-rolled thousands separator, or `+ 's'` for a plural in a screen. Use `fmt.ts`.
- A time with no id beside it, an ISO string in a row, and "just now".
- `pl-*` / `pr-*` / `left-*` / `right-*` / `text-left` in layout, and any fixed-width text container.
- A toast for a fault that persists. If it has to be read, it is a Callout.
- A toast and a Callout for the same event. A red dot with no count.
- A disabled control with no reason beside it. A save button disabled because the form is invalid.
- Validation on every keystroke, before the field has ever been left.
- "Success", "Done" or "Error" as the whole message.
- A stored secret prefilled into an input. A banner that pushes the page down.
- Pie charts, donuts, treemaps, stacked areas. A truncated y axis on bars.
- A colour legend, or two series told apart by `--chart-1` / `--chart-2`.
- A chart with no table view, or more than four series on one plot.
- Hover-only readouts (the `GeoMap` desktop pointer readout is the one exception, because the list beside it carries the keyboard).

## Type

- `.op-display` 800 — landing hero, one per page, never in the console.
- `.op-h1` 700 — landing major section title.
- `.op-h2` 600 — minor section or panel title; the console's largest tier.
- `.op-title` 700 — console page title; the one 700 line on a screen.
- `.op-h3` 600 — item title in a grid, section title inside a page.
- `.op-lead` 400 muted — the sentence under a title.
- `.op-label` 500 uppercase tracked — eyebrow, column header, key badge. Never a section title.
- Give one page one display headline. Two biggest things means neither leads.

## Motion

- Move a control's own state, a drop opening, a row entering focus, a live value updating. Nothing else.
- Never move layout, a page transition, a chart drawing itself, or a skeleton shimmering.
- Use `--op-duration` (100ms) by default, `--op-duration-fast` (80ms) for hover, `--op-duration-slow` (200ms) only for something arriving on top of the page.
- Use one curve, `--op-ease`. Change the tier with `.op-motion-fast` / `.op-motion-slow`, never with a literal.
- Reduced motion is one media rule in `op.css` that zeroes all three durations. Never gate motion in JavaScript.
- Leave the two exceptions alone: `.op-raise`'s hard 3px offset never lifts, and `animate-pulse` / `animate-spin` are the only surviving animations.

## Status vocabulary

- `ok` ● success — healthy, passing, deployed.
- `warn` ◐ warning — degraded, above threshold, expiring.
- `error` × destructive — failing, unreachable.
- `idle` ○ muted — not deployed, not configured, nothing yet.
- `sampled` ◌ muted — head-sampled past the plan allowance.
- Order lists with `STATE_RANK`. Pick the page glyph with `worst(states)`.
- Use an icon for what a thing or event *is*; use a glyph for what state it is in.

## Icons

- Give every mixed-kind list a kind icon: palette pages and resources, databases
  by engine, nodes by role, providers, settings rows.
- One mark before a name. A row that carries an identity mark (a project's
  mark) gets no kind icon; its kind is a word in the meta (`worker · production`).
- Put it in a fixed 16px slot (`size-4 shrink-0`) before the name, in muted ink.
- Keep the state glyph in its own slot. Icons and glyphs never share one.
- Leave the icon off a single-kind list whose title already names the kind.
- Use `LedgerRow.icon`, `PickerOption.icon`, the `Breakdown` row `icon`, and the
  leading icon on a palette `CommandItem`. Never colour an icon.
- Use lucide only, stroke 1.75, `size-4` in a row and `size-3.5` in a label or button.
- Give a concept one icon and one only. Check the table in `docs/icons.md` before adding a second.
- Add a concept in one PR: one row in the `docs/icons.md` table, plus a real call site.

## Page structure

- Choose the layout from the data and the operation, not from habit:
  - many records of one kind → `Ledger` (one per screen; owns `/`, `j`/`k`/`⏎`, the footer).
  - two kinds of record → two facets, a tab each. Never two ledgers stacked.
  - one record read top to bottom → `Detail` + `Columns`, no tabs.
  - one resource with 2–6 facets → `Detail` with tabs, one row.
  - a configuration → `Settings` with sections and a sticky save bar.
  - nothing yet / not set up / failed → `PageState`.
- Give a page one row of tabs, ever. A scope is a `Picker` read as a sentence ("in production").
  2–4 views of one list are a `Segmented` in the toolbar. Time is a `RangePicker`.
- Order a record: title + meta → status (verdict) → `Lede` → `Columns`( main: content then events · aside: reference ).
- Make every section one `SectionTitle` (600/14 + one mono fact) and exactly one body.
- Separate sections with an ink rule; frame every group; raise exactly one thing.
- Let every block share the page's left and right edges. Cap the measure inside the frame (~70ch), never the frame.

## Record page checklist (enforced by `scripts/audit-records.mjs`)

1. `meta` places the record: id · project · environment. Never the id alone.
2. The verdict says what to do, or "Nothing to do: …" with the proving fact. Never repeats the Lede word.
3. `Lede` carries four to six `facts`. A sentence alone is a headline, not a lede.
4. A fact appears once. The aside is what is left after the meta and the Lede.
5. Main column is the thing and its timeline; the aside is `KeyValue` and lists of ≤5.
6. No tabs on a single record unless a facet is its own list or tool.
7. Actions do, facets go. Nothing in the actions row may only switch tab.
8. A drawn control is a wired control. Typed destinations (`/${string}`), never `#`.

## Forms

- Give every control a `Field`: visible label at 500, hint, control, error. A placeholder is an example, never a name.
- Validate a field on blur, the form on submit, and a field already in error on every keystroke until it clears.
- Write an error as a state word and a sentence that names the resource and the fix. Never "invalid" alone.
- Put the message under its field; add a `FormErrors` Callout only when more than one field fails, each entry focusing its field.
- Mark the exception: the console's forms are mostly required, so mark `optional` and never "required".
- Never disable a control without the reason beside it. A control that needs configuration onboards; it does not disappear.
- Give a form one save: the `Settings` sticky bar, `save ⌘S`, discard beside it while dirty, "no changes" after.
- Keep a long submit on the form: progress on the button, fields locked, never a spinner page, nothing typed thrown away.
- Route destructive submits through `EchoDialog`; ask for the typed echo, and use red, only when the loss is irreversible.
- Never prefill a stored secret. `SecretValue` shows it is set, with reveal and copy; replacing says what breaks.
- `⏎` submits a single-field form only. `esc` closes what is open and discards nothing.

## Notifications

- One surface per message: verdict → `StatusLine`; fault in context → `Callout`; result of an action → toast; missed while away → the bell; blocking decision → `EchoDialog`.
- Write a toast as state · headline · fact, six words or fewer, naming the object: `api-gateway deploying · dep_93a`.
- Use `ok` `warn` `error` and their glyphs (● ◐ ×) and no other severity words.
- Count unread by state in the bell, with a number. Quiet is one green glyph and nothing else.
- Let nothing move the layout to speak. The `Settings` sticky save bar is the only exception.

## Data

- Time: relative under a day (`41m ago`), absolute after, with the deploy id beside it.
- Put deploy markers on every time axis. Make every delta name its baseline.
- Empty value is an en dash. Zero is `0`.
- A chart with no data says which of four reasons: no traffic, not configured, sampled, past retention.
- State the retention horizon in the chart footer. Strike gated ranges through, never hide them.
- Render logs with `LogViewer` / `LogLines`, never a `<pre>`.

## Charts

- Pick the chart from the question: over time → `TimeChart`; share or rank → `Breakdown`; steps → `Funnel`; from→to → `Flow`; distribution → `Histogram`; by bucket → `StatusStrip`; 0–100 → `ScoreRing`; by day → `CalendarHeatmap`; nested timing → `Waterfall`; by country → the ranked list, `GeoMap` second.
- Separate series by pattern, never hue: `stroke` solid · dashed · dotted, `weight` thin · regular.
- Let `TimeChart` draw the legend from `series`. Never type one in a footer.
- Give a series a tone only when the series is itself a state (`series.state`): an error rate against its threshold band.
- Start count axes at zero, and never truncate the y axis on bars.
- Label a log scale `log` on the axis, or do not use one.
- Give every chart `role="img"` and an `aria-label` sentence built from `title`, `range` and `verdict`.
- Ship a table view with every chart (`TimeChart` `table`, on by default) and make the readout row navigable with `←` `→`.
- Put touch readouts under the chart; hatch a partial bucket and say so.
- Say the unit once in the header, never on every tick; numbers stay mono and tabular.
- State range · retention · sampled · the baseline of every delta in `ChartFooter`.
- Keep one plot to four series. More is small multiples or a table.

## Content

- Write everything in sentence case. Spell product names as their owners do; never re-case an identifier.
- Use one term per concept: deployment, project, environment, node, provider, backup, variable, issue, run, member. No synonyms.
- Say `roll back` for the verb and `rollback` for the noun; `sign in`, never `log in`.
- Say `remove` when the thing survives and `delete` when data dies. Only `delete` goes red.
- Shape every error as what failed · on what · why · what to do next, with the id. Never "something went wrong".
- Quote the other system verbatim in mono; translate and paraphrase nothing a machine wrote.
- Say what did not change when nothing did ("Staging stayed on dep_89f").
- Label a button verb first, object second. Never `OK`, `Yes`, `Submit`, `Confirm`, `Done`.
- Name the loss in a destructive action ("Delete project and 14 backups"). Say how to undo it when it can be undone.
- Write empty states as fact then next step; write unconfigured states as what is missing, an example, and the link that fixes it.
- Separate facts with a spaced middle dot (`·`). No trailing period on a label, cell, button or tab. No exclamation marks. `…` only for truncation.
- Format numbers, percentages, bytes, durations, counts and times through `fmt.ts`. Nothing is `–`; zero is `0`.
- Give every relative time a `title` with the absolute stamp, and an id beside it.

## Locale

- Design labels at 130% and buttons at 200% of their English length; never fix the width of a text container.
- Build no sentence by concatenation: one template per sentence, named slots, `fmtCount` for every count.
- Use logical properties (`ps-`/`pe-`/`ms-`/`me-`/`start`/`end`/`text-start`) in layout; flip direction icons, never thing icons.
- Keep charts, logs, code, ids and stack traces LTR and untranslated; translate the sentence around the quote, never the quote.

## Keyboard

- `⌘K` palette · `/` filter · `j` `k` `⏎` ledger · `1` `2` `3` tabs · `⌘⏎` primary · `⌘S` save · `esc` close.
- Ignore every key while an input has focus.
- Move DOM focus with the cursor. Never paint a highlight without moving focus.
- Give every key a visible badge, and every badge a handler. A shortcut is an accelerator for a control you can see; if the control goes, the key goes with it.

## Responsive (390 and 1440 are both required)

- Route actions through `ActionBar`; below sm they scroll sideways at natural width, never stack full-width.
- Ledger rows hide `cells` below md and render `mobile`, which must carry the row's primary action.
- Scroll tab strips and action bars with `ScrollRow` / `.op-scroll-x`; never wrap into two rows.
- Use `.op-tiles` with `--tiles: N`; phones pair tiles two per row, odd last tile spans.
- Never let the document scroll sideways at 390. Deliberate scrollers only.
- `GeoMap` reads the hovered country at the pointer on desktop, never in a row under the map. Below md the row under the map is the reader: tap to read, tap again to open.

## Before you ship

- `bun run lint` (tsc + `scripts/audit-records.mjs` + `tokens.mjs check`) is clean.
- `bun run e2e` is green: overflow at 390 and 1440, keyboard, drop focus, axe light and dark, visual baselines.
- Look at the screen at 1440 and 390, in light and dark.
- Fix dev warnings from `Lede` (fewer than three facts) and `Detail` (lede without meta or status).
- Change a rule only by editing the document that owns it, this file and the reference page in one commit.

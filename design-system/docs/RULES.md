# Temps design system: rules for agents

Machine-readable digest of `brand-guidelines.md` and `design-system-handoff.md`.
Imperative only. When this file and those two disagree, they win — fix this file.
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

## Banned

- Tailwind palette literals (`text-red-500`) and hex in tsx.
- A second hue. `data-accent` is landing-only, one filled element per viewport.
- Spinners as page state. Use `PageState state="loading"` (skeleton rows).
- Blank empty states. Every non-happy state is `PageState`.
- Confirm dialogs that are not `EchoDialog`.
- `<select>` for branches, images, regions, environments, or >7 options. Use `Picker`.
- Titles at weight 500. Titles 600–800, body 400, labels 500.
- Cards as layout. Stock shadcn card look.
- Sparkles, gradients, wands, the word "magic", `Loader2` as a page.
- Hiding a feature because it is unconfigured. Show it, say what is missing, link the fix.
- Hand-rolled hovers on filled controls. Use `.op-fill-ink` / `.op-fill-destructive`.
- `href="#"` with `preventDefault`. A `Kbd` badge with no handler. A filter that filters nothing.
- Tabs on a single record. Tabs inside tabs. Two `Ledger`s on one screen.
- Red on a confirmation that is reversible. Red means irreversible loss only.
- A mixed-kind list with no kind icons.
- A kind icon in the state glyph's slot, or a kind icon carrying a state colour.

## Type

- `.op-display` 800 — landing hero, one per page, never in the console.
- `.op-h1` 700 — landing major section title.
- `.op-h2` 600 — minor section or panel title; the console's largest tier.
- `.op-title` 700 — console page title; the one 700 line on a screen.
- `.op-h3` 600 — item title in a grid, section title inside a page.
- `.op-lead` 400 muted — the sentence under a title.
- `.op-label` 500 uppercase tracked — eyebrow, column header, key badge. Never a section title.
- Give one page one display headline. Two biggest things means neither leads.

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

## Data

- Time: relative under a day (`41m ago`), absolute after, with the deploy id beside it.
- Put deploy markers on every time axis. Make every delta name its baseline.
- Empty value is an en dash. Zero is `0`.
- A chart with no data says which of four reasons: no traffic, not configured, sampled, past retention.
- State the retention horizon in the chart footer. Strike gated ranges through, never hide them.
- Render logs with `LogViewer` / `LogLines`, never a `<pre>`.

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

- `bun run lint` (tsc + `scripts/audit-records.mjs`) is clean.
- `bun run e2e` is green: overflow at 390 and 1440, keyboard, drop focus, axe light and dark, visual baselines.
- Look at the screen at 1440 and 390, in light and dark.
- Fix dev warnings from `Lede` (fewer than three facts) and `Detail` (lede without meta or status).
- Change a rule only by editing `brand-guidelines.md`, `design-system-handoff.md`, this file and the reference page in one commit.

# Temps design system: rules for agents

Machine-readable digest of `brand-guidelines.md`, `design-system-handoff.md`,
`content.md`, `localisation.md`, `forms.md`, `notifications.md`, `data-viz.md`,
`generative-ui.md`, `motion.md`, `icons.md` and `requirements.md`.
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
- Make every border ink on paper (`--border` equals `--foreground`). On night, borders and the raise are 62% ink: a light stroke on a dark ground weighs more than an ink stroke on paper. Use `--op-rule-soft` only for row dividers.
- On night, keep subdued regions darker than the ground and state hues at chroma 0.11–0.14. Never invert the light pair and stop there.
- Ship no cards. Use one `.op-raise` per screen, on the thing the reader must act on.
- Emit colour only through `Status`: glyph, word, tone, in that order. Never a bare tone.
- Keep whitespace between sections, not inside tables.
- Freeze radius at 0.25rem, borders at 1px, spacing at 4/8/12/16/20/24/32px.
- Use Geist and Geist Mono. No other faces.
- Set numbers in mono, tabular, unit after the value in muted.

## Requirements

- Put every piece of view state in the URL: tab or facet, filter, sort, page, range, inspected row, columns. The path names the record, the query names the view of it.
- Rebuild the page from its address alone. A reload, a pasted link and a second tab produce the same layout and the same data.
- Prove it with a reload signature: title + heading + active facet + first three row ids + range label, before and after a reload. Identical, or it is a bug.
- Derive every fetch from the path and the params. The same URL fetches the same thing; nothing is fetched from component-local state.
- Omit defaults. Writing the fallback deletes the key, so the common address carries no query at all.
- Replace on a view change, push on a navigation. Typing a filter must not fill the history one keystroke at a time.
- Write several keys in one patch. Two `setParams` calls in one handler compute from the same snapshot, so the second drops the first.
- Emit complete links: every row, every "open in …", and `copy link` carries the view the reader is on, not the bare record.
- Keep only the moment local: a hover, an open menu, a two-second `copied`, an unsubmitted draft — and say so before a reload would lose the draft.

## Tokens

- A hover or a selection lifts the whole row: muted text, state glyphs and icons step up with the fill, never dimmer under the reader. One variable (`--muted-foreground`) does it, so nothing is left behind.
- Take every value from a token. A hex, an `oklch()` or a `ms` literal in a tsx file is a bug.
- Name only semantic tokens in a component (`--muted-foreground`), never a base token.
- Edit `web/packages/op/tokens.json` and `op.css` in one commit. `bun run lint` fails when they disagree.
- Add a token to light; add it to dark only when the value actually changes. Dark cascades.
- Keep the scale closed: radius 0.25rem, borders 1px, spacing 4/8/12/16/20/24/32, six type tiers, three durations.

## Banned

- View state that only lives in React state: a tab, a filter, a sort, a page or a range you cannot link to.
- A link that drops the view: a row or a `copy link` that opens the record without the facet, filter and range the reader was on.
- Tailwind palette literals (`text-red-500`), a hex or an `oklch()` in tsx.
- A literal duration in a tsx file. `duration-150`, `transition-all`.
- A second hue. `data-accent` is landing-only, one filled element per viewport.
- Spinners as page state. Use `PageState state="loading"` (skeleton rows).
- Pulsing an `ok` glyph.
- Pulsing a live label.
- A spinner that is not on a busy button. `Loader2` as page state, a spinner beside a row, a spinner in a header.
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
- A toast for a copy, or any answer the pressed control could have given itself. A button that does something and says nothing.
- A disabled control with no reason beside it. A save button disabled because the form is invalid.
- Validation on every keystroke, before the field has ever been left.
- A calendar as the only way to enter a date. A time with no zone beside it.
- An empty date that means forever. Free-text durations (`30d` in a text box). `MM/DD/YY`.
- "Success", "Done" or "Error" as the whole message.
- A stored secret prefilled into an input. A banner that pushes the page down.
- Pie charts, donuts, treemaps, stacked areas, Sankey and alluvial diagrams, radial gauges and needles, force-directed graphs. A truncated y axis on bars.
- A colour legend, or two series told apart by `--chart-1` / `--chart-2`.
- A legend that says "less … more", or any swatch with no number behind it.
- A colour ramp or a red-to-green scale for a count: density is how much, not how well.
- A fifth layer on one composition, or hue used to separate layers.
- A delta with no baseline, or a red delta on a metric with no budget.
- A red area flood or a shaded rectangle behind a plot.
- A bar pinned at 100% with the overage invisible.
- A restore form with no picture of the window it accepts.
- A graph with no list beneath it, a layout that moves between reloads, or a focusable control inside a `role="img"`.
- A chart with no table view, more than four series on one plot, or a figure whose data is a series and which ships no table view.
- Hover-only readouts — an anomaly, a map country or a heatmap cell reachable only by hovering (the `GeoMap` desktop pointer readout is the one exception, because the list beside it carries the keyboard).
- Chat bubbles, avatars, an "AI" circle, typing dots.
- A markdown table where a `Ledger`, `KeyValue` or `Breakdown` exists.
- A chart, list or number an agent rendered with no `Provenance`.
- An agent confirming its own proposal, or a write that ran without a human.
- A hidden, collapsed-away or summarised-over tool call — especially a failed one.
- A free-text "please provide …" where typed options or a `Field` belong.
- A `StreamBlock` that is a different shape or height from the block that lands.
- An external link from a tool that was not a web search.
- A hero illustration, stock photography, or a browser-chrome mockup around a screenshot.
- A rounded or shadowed image. A screenshot with no frame, no caption or no alt.
- A screenshot shipped on one ground only, when the product has two.
- A picture carrying a fact the prose does not say.
- A code block a reader has to retype, and a prose measure that is not capped inside its frame.

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
- Leave the exceptions alone: `.op-raise`'s hard 3px offset never lifts, and `animate-pulse` (skeletons), `.op-pulse` (a `running` glyph) and `.op-busy` (a button doing the work) are the only surviving animations.
- Give a `running` glyph a slow opacity-only pulse (`.op-pulse`, ~1.6s), zeroed under `prefers-reduced-motion`. It is the only motion in the system that is not a control answering the reader, because motion means work is happening *now* and it stops when the work stops.
- Give a button whose work is still running `busy` and `busyLabel`: it spins its own icon at 0.9s, says the verb in progress, keeps its width, its colour and its focus, and is never `disabled`. A reload spins because the thing it stands for goes round; a state pulses, because a state is not an action. Nothing else in the system spins.

## Status vocabulary

- Six states, and no seventh: `ok` ● · `warn` ◐ · `error` × · `running` ◉ · `idle` ○ · `sampled` ◌.
- `ok` ● success — healthy, passing, deployed.
- `warn` ◐ warning — degraded, above threshold, expiring.
- `error` × destructive — failing, unreachable.
- `running` ◉ ink, never a hue — work in flight. It is not a verdict, so it takes no tone.
- `idle` ○ muted — not deployed, not configured, nothing yet.
- `sampled` ◌ muted — head-sampled past the plan allowance.
- Take a `running` word from the operation: building, restoring, scanning. Never "in progress".
- Say pending / waiting-for-you as `warn`, not `running`, and warn does not pulse. Nothing is happening; somebody has to act.
- Order lists with `STATE_RANK` (`running` ranks between warn and sampled). Pick the page glyph with `worst(states)`.
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
- Changing a tab or a facet never moves the document; only the content below the strip changes. The strip reveals its active tab sideways, and the reader's place on the page is theirs.
- Give a page one row of tabs, ever. A scope is a `Picker` read as a sentence ("in production").
  2–4 views of one list are a `Segmented` in the toolbar. Time is a `RangePicker`.
- Order a record: title + meta → status (verdict) → `Lede` → `Columns`( main: content then events · aside: reference ).
- Make every section one `SectionTitle` (600/14 + one mono fact) and exactly one body.
- Separate sections with an ink rule; frame every group; raise exactly one thing.
- Let every block share the page's left and right edges. Cap the measure inside the frame (~70ch), never the frame.

### Logs and tools

- A tool screen is one list with a query bar in front of it: no tabs, no record, and the query bar owns `/`.
- Tokens are the truth. Every facet, scope `Picker`, saved query and verdict `Phrase` writes one; nothing narrows a list by a state the reader cannot see, remove, or copy.
- Put the query in the URL. A search that cannot be linked is a search that has to be typed again.
- Facets are `Breakdown`s that add tokens, counted over the window and not over the query, so the reader can always widen.
- Give a list two to four renderings, never two lists: one `Ledger`, one keyboard, different columns (raw · grouped by pattern · ranked by owner).
- Live tail pins the newest line while the reader is standing where new lines land; scroll away and it holds and counts ("paused · 12 new · resume"), never moves the ground under them.
- Correlation is inline and goes both ways: a line shows the trace it belongs to and the request it served; a trace shows its lines. A line with no trace says so as a fact and links the setting that turns tracing on — never a section that quietly disappears.
- Inspect a row in an `Inspector` beside the list, never by leaving it: `⏎` opens the panel, `j`/`k` keep moving the ledger's cursor and the panel follows, `esc` returns focus to the row, and `/` still belongs to the query bar. The record page is the deep link; the panel is how you stay in the list.

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

### Dates and times

- Enter every date and time by typing, in a native `date` / `time` / `datetime-local` under the ink skin; the picker is the accelerator.
- Write every stamp ISO-ordered through `fmtStamp`: `2026-09-06 20:33`. Seconds only where the operation is second-precise.
- Put the zone beside the control as a mono fact, always. Change it with a `Picker` in the same `Field`.
- Fill the absolute field from the preset strip (`now`, `−1h`, `last backup`); the field stays the truth.
- Write a range as `from` → `to`, validate `to > from` on blur of `to`, and strike gated windows through with the plan word.
- State the window once in the hint and refuse outside it with the state word and the fact.
- Show a schedule's next three runs under the field; cron is the advanced entry beside the simple one, never the only one.
- Enter a duration as a number plus a unit `Picker` (`s` `min` `h` `d`) and display it with `fmtDuration`.

## Notifications

- One surface per message: verdict → `StatusLine`; fault in context → `Callout`; result of an action → toast; missed while away → the bell; blocking decision → `EchoDialog`.
- Every action answers, and it answers where the reader is looking. A control that says what it did (`copied`, `saving…`, `noted`) answers on itself; only an action whose result is elsewhere (a deploy started, a row removed) gets a toast. A press with no answer is an unwired control.
- A copy is `CopyAction`: it writes the clipboard, says `copied` on the button for two seconds, or `couldn't copy` in red with the reason. Never `notify('ok', 'copied')`: a toast lands in a corner and says copied whether or not anything was.
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

- Pick the chart from the question: over time → `TimeChart`; share or rank → `Breakdown`; steps → `Funnel`; from→to → `Flow`; distribution → `Histogram`; by bucket → `StatusStrip`; 0–100 → `ScoreRing`; by day → `CalendarHeatmap`; nested timing → `Waterfall`; by country → the ranked list, `GeoMap` second; is this normal → `BandChart`; against the period before → `TimeChart` `compare`; composition over time → `StackedInk`; one slow route or all of them → `LatencyHeatmap`; a distribution in a tile → `PercentileLadder`; do they come back → `CohortGrid`; what next → `PathTree`; a session → `SessionTimeline`; against an allowance → `UsageBar`; a machine's pressure → `Gauge`; how long was it down → `StateTimeline`; before and after → `DeltaTable`; what a restore can reach → `WindowTimeline`; what talks to what → `Topology`.
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
- Draw an expected range as a hatched ink band behind the line (`TimeChart` `band`), never as a filled area or a background wash; colour the out-of-band **segment** in the state tone (it is a state), put a × at its peak, derive the excursions from the data, and list them under the plot as well as summarising them in the footer with the deploy beside.
- Draw the prior period as a dotted thin ghost (`compare`), put the delta in the generated legend with its baseline, and compare equal-length windows only.
- Draw composition over time as stacked **bars** from zero, at most four layers told apart by pattern at ≤3 greys; state tone only on the layer that *is* a state.
- Give a density grid the five ink steps and a legend that prints the numbers behind the swatches; zero is the empty step, never a light something.
- Put a delta's baseline on its own row (`PercentileLadder`, `DeltaTable`, `Metric`), and tone it only when a threshold makes the value a state.
- Draw retention as a real `<table>` with the percentage in the cell and the cohort size in a column; a period not yet reached is an en dash.
- Draw journeys as an indented, collapsible tree with drop-off per branch.
- Put a session's events in a list beside the axis; the list carries the keyboard, the scrubber is a native range input.
- State usage as a sentence before the bar, hatch the overage, mark where sampling began, and always name the plan and its allowance.
- Draw a resource gauge horizontally from zero with threshold ticks that carry their own words and the peak with its window; an unsampled node keeps its tiles and says why.
- Use `StatusStrip` in a ledger (equal buckets, rows compared by shape) and `StateTimeline` on a record (real transitions with durations); never both for the same window on one screen.
- Put the recoverable window on the same axis as the restore cursor, with the zone printed, directly above the point-in-time field.
- Lay a graph out in deterministic layers and put the same nodes in a list beneath it; the list carries the keyboard, and the graph is one `role="img"` with no focusable children.
- Give every density and map figure a readout that works on a pointer, on touch and on a keyboard — including `CalendarHeatmap`, whose cells used to carry only a `title`.

## Generative UI

- Answer with the console's blocks: over time is `TimeChart`, a set is `Ledger`, one record is `Detail`, facts are `KeyValue`. An agent has no drawing kit of its own.
- Render nothing the console does not already have. A missing block is a PR, not an improvisation.
- Make every step a `ToolRow`: kind icon · state word · title · meta · duration, collapsed, opening to input and output.
- Use the seven state words and no others: preparing, running, done, failed, needs approval, approved, denied.
- Show a failed call as `×` and the error verbatim in mono. Never hide it, never summarise it away.
- Hang `Provenance` under every generated block: `from <tool> · <when> · <range>`, with `show query`.
- Render reads immediately; propose every write. A `Proposal` states action · target · consequence · reversibility, and nothing runs until a human confirms.
- Confirm a reversible write in ink; route an irreversible one through `EchoDialog` with the name typed out. Red means loss nobody can get back.
- Say the autonomy level per capability in words — observe · propose · act with approval · autopilot — on the proposal and in the run aside.
- Stream in four states only: thinking with seconds, a call running with a live duration, partial text with `.op-caret`, and a `StreamBlock` shaped like the block that is coming.
- On stop, keep what completed and say what did not: "Stopped after step 4 of 9."
- Ask with `AgentQuestion` typed options, pick then confirm; ask for missing parameters with `Field`s, never with a sentence.
- Answer a failure with the fix: name the permission and link the setting, state the retry, state the quota. Never "I don't have access to that".
- Lay it out as conversation (main) · run (aside: model · workspace · mode · context · checkpoints) · composer (fixed bottom); below md the aside becomes the composer's picker row.
- Write verdict first, then the blocks; sentence case, one fact once, no "I have successfully…".

## Monitoring

- Open a monitoring page with a verdict: the container to move, not the number it reached.
- Put cpu, memory, disk and network on one time axis with one cursor; `←` `→` walk the buckets and every readout answers for the same one.
- Print a percentage beside its absolute, always: `92% · 7.4 GiB of 8.0 GiB`.
- Draw a threshold as a dashed line carrying its own word (`busy 80%`, `oom risk 95%`), and tone only the stretch that crossed it.
- State the projection on anything that fills: the free space, the rate, and the date it runs out.
- Attribute pressure under the chart: a ledger of containers worst first, `⏎` opening what the reader can act on.
- Order every fleet list pressure first — unreachable, then closest to a threshold — and fold a service's resources into its overview, never into a seventh tab.
- Keep a silent node's tiles and stamp each with the time it was last true; never a gauge with no number behind it, and never an empty page where a sick machine should be.
- Pause a live number the moment the reader scrolls, and say `paused`.

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

## Content pages

- Dress every body of long-form prose with `.op-prose` — a post, a docs page, a changelog entry. One class, one copy of the rules.
- Use `Article` for a page that is read top to bottom; `Detail` is for a record the reader came to act on.
- Cap the measure inside the frame (`--op-measure`, ~68ch). Figures, tables and code panes are allowed past it.
- Give an `Article` a table of contents as soon as it has two `h2`s, built from the rendered headings, and every entry a real link.
- Frame every screenshot at 1px, caption it (`fig. 3 · …`), and ship it as a light/dark pair through `ImageFigure`.
- Write the alt as what the picture shows and the caption as what to notice. Both, always.
- Make every code block copyable: `CodeBlock` says the language, the filename and copies on itself.
- Write a table as a ledger: `op-label` header, 1px rules, numbers right through `data-align="end"`.
- Wrap a live block dropped into prose in `.op-raw`. It is a component, not prose.

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

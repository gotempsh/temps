# Temps design system: handoff

This is the document to read first. It is written for whoever picks the work
up next, human or model, and it is complete enough to continue without the
conversation that produced it. Everything it describes exists in
`temps/design-system/` and can be run and looked at.

Companion documents, in reading order after this one:

- `brand-guidelines.md`: the direction, the type scale, colour, signature moves.
- `design-system-answers.md`: the twelve questions a design system must answer,
  answered for Temps from the pricing page, the console source and the product.
- `operator-console-brief.md`: the original brief. Historical. Do not edit.

## 0. How to run and look

```bash
cd temps/design-system
bun install
bun run dev --port 5183 --host      # http://localhost:5183
bunx tsc --noEmit -p .              # must be clean before any hand-back
```

The reading entry point is `/guide`: one page, in the same chrome as every
reference page, that renders these markdown files — this one, `brand-guidelines.md` and `ux-audit-2026-09-06.md` —
in the order someone building a screen needs them, with live token swatches,
the type scale in its real classes, the five status glyphs and an example
primitive beside the rule it illustrates. The markdown files stay the single
source of truth; the guide never copies their text, it imports them with Vite's
`?raw`. Edit the document, not the page. `docs/RULES.md` is the compact,
imperative digest of the same rules, written to be pasted into a coding agent's
context before it builds a screen; it is rendered at `/guide#tooling` and it is
not authoritative — this file and `brand-guidelines.md` win.

Routes that matter:

| Route              | What it is                                                             |
|--------------------|------------------------------------------------------------------------|
| `/guide`           | The consolidated guide. One page over these documents. Read this first. |
| `/v1`              | Operator console v1. The reference implementation. Start here.         |
| `/v1?p=api-gateway`| Project detail: chart, metrics, incident thread, settings tab.         |
| `/v1?p=settings:nodes` | Fleet: the nodes ledger with a status column (`hetzner-3` offline, `hetzner-1` under memory pressure); `node:<name>` the record; `settings:cluster` the join token, cluster DNS and CA. |
| `/v1?p=deploy:dep_91a` | Deployment record: `dep_91a` live with an error-rate regression, `dep_92e` failed build with the compiler's words, `dep_92b` building live, `dep_90e` superseded (roll back), `dep_88c` cancelled. |
| `/v1?p=errors`     | Issues ledger; `issue:<id>` the record. `&fail=1` shows the error-store outage with retry; **fresh** shows the no-DSN onboarding. |
| `/console?p=…`     | The console alone, no sandbox layout or intro; same `p` views. The ⤢ button in the header toggles it. |
| `/landing`         | The landing alone, no sandbox layout. The ⤢ button fixed bottom-right toggles it (sandbox control, not part of the page). |
| `/v1?p=settings`   | Settings hub; `settings:<slug>` pages (domain, updates, builds, timeouts, users, teams, signin, keys, headers, traffic, routes, store, retention, alerts, nodes, plugins). |
| `/status?project=` | The public status page for a project, chrome-free; `/status-page` inside the sandbox. `/v1?p=monitor:mon_2` is a monitor record. |
| `/v1?p=sandboxes`, `?p=sandbox:sbx_7f21`, `?p=traces`, `?p=trace:3f9c1e7a8b2d4f60`, `?p=metrics` | Observe and sandbox surfaces (§7b). |
| `/v1-landing`      | Landing page in the same system, with pricing.                         |
| `/op-components`   | Every operator component, every state, with props.                     |
| `/brand#hierarchy` | The type scale rendered live.                                          |

Gotchas that cost time:

- Tailwind only generates classes it has seen. After adding a class that is new
  to the codebase, restart the dev server or it will silently not apply.
- Dialogs, toasts and command palettes render in portals outside the
  `.operator` root. Pass the skin class (`operator ink v1`) to their content.
- `data-accent` is set on the landing root only. The console has no accent.
- Screenshots: `agent-browser` works well. Set the viewport, open, wait, capture.

### The package

The primitives are no longer the sandbox's: they live in
`temps/web/packages/op` as **`@temps-sdk/op`** (a bun workspace of the
console), with the tokens and every `.op-*` rule in `op.css`. The sandbox
consumes it through a Vite alias (`vite.config.ts`) plus `resolve.dedupe`
for react/react-dom/react-router and a `paths` pin for the React types in
`tsconfig.json`; without both you get a duplicate React at runtime and
incompatible `CSSProperties` at type level. `src/components/op/index.ts`
is a one-line re-export so `@/components/op` imports keep working.

A consumer imports `@temps-sdk/op/op.css` at the top of its stylesheet
(imports must precede rules) and puts `operator ink` on the root element
it wants skinned; the skin is scoped to `.operator`, nothing outside it
changes. Version and changes: `web/packages/op/CHANGELOG.md`.

### Tests

Playwright, Chromium only, in `e2e/`. It reuses a dev server already on the
port it is pointed at (5183 by default, `DS_PORT` to change it) and starts one
only if nothing answers, so leave your tab open.

```bash
bun run e2e          # the whole suite (~1 min)
bun run e2e:ui       # pick and step through tests interactively
bun run e2e:update   # rewrite the visual baselines
bunx playwright test e2e/keyboard.spec.ts --project=desktop   # one file, one width
bunx playwright show-report                                   # last run's HTML report
```

Two projects: `desktop` (1440×900) runs everything, `phone` (390×900) runs the
layout suites only.

| Spec | Checks |
|------|--------|
| `keyboard.spec.ts` | §9: `j`/`k`/arrows move **focus** onto the marked row, `⏎` opens it, `/` focuses the filter, `[`/`]` page, digits switch facets, every key ignored inside an input. |
| `drop.spec.ts`     | The header attention panel opens, closes on Escape and on an outside click, and returns focus to its button; tooltips open on hover with no `animation-name` and close when the pointer leaves. |
| `overflow.spec.ts` | Every route in both widths: no horizontal document scroll, and a clean console (Vite/HMR noise filtered). |
| `a11y.spec.ts`     | axe-core over the main surfaces in **light and dark**. Serious/critical fail unless the rule is in the documented `KNOWN` list at the top of the file; moderate and minor land as test annotations. |
| `visual.spec.ts`   | One snapshot per `/op-components` block plus full-page shots of four records and the settings hub, in desktop light, desktop dark and phone light. |

Updating snapshots: run `bun run e2e:update`, then **look at the diff before
committing it** — that is the whole point of the baseline. Only regenerate from
a quiet dev server (no HMR error overlay, `bun run lint` clean), or you bake a
half-finished refactor into the baseline. Baselines live in
`e2e/__screenshots__/` and are committed; `test-results/` and
`playwright-report/` are not.

`a11y.spec.ts`'s `KNOWN` map is a debt register, not a mute button: each entry
names the rule and what causes it, a test fails if an entry stops firing (so
fixed ones get deleted), and any *new* serious violation fails immediately.

## 1. What this is and is not

It is a token layer and a small component library that sit on top of the
console's existing shadcn primitives (`temps/web/src/components/ui`). It is
applied by putting `operator ink v1` on a root element. Nothing under that
root needs rewriting to pick up paper, ink, mono numerals and 0.25rem radius.

It is not a fork of shadcn and not a new component kit. The console has 516 tsx
files and 117 pages; the only viable path is reskin by tokens, then replace
screens one at a time with the templates in §7.

## 2. Who it is for

From `temps-landing/public/pricing.md` and `temps/CLAUDE.md`:

- Self-hosted is free, unlimited users, on a $5–10 VPS. The reader operates the
  box, alone, with no support channel. The console is the only help they have.
- Cloud is $29 / $99 / $299 per month with no per-seat fees, plus Enterprise.
  The buyer is the same person who operates it, later a team lead who has to
  justify the bill and prove things happened.

So the reader is an operator at a bad hour. The landing talks to that person
justifying Temps to a team. When the two conflict, the console wins.

The emotional job is "I can see what is wrong and what to do about it". Not
"this looks modern".

## 3. The five rules

Everything in the system follows from these. If a change violates one, the
change is wrong, not the rule.

1. **Paper and ink only.** Background is warm off-white, text is near-black.
   Dark mode inverts the same pair. No greys for structure.
2. **Every border is ink.** 1px `--border` equals `--foreground`. The only
   exception is row dividers inside a ledger, which use `--op-rule-soft`
   (16% ink).
3. **No cards.** One raised element per screen (`.op-raise`, a 3px hard
   shadow). It is the thing the reader is meant to act on.
4. **Colour means status.** Green, amber, red appear only through the `Status`
   component, next to a glyph and a word. The focus ring is blue on focus only.
   The single landing accent lives on `--primary` and appears once per viewport.
5. **Dense by default.** Whitespace is spent between sections, not inside
   tables. Density has two settings and the choice is remembered.

## 4. Tokens

All in `op.css`, shipped with the components in `@temps-sdk/op`; the sandbox's
`src/globals.css` imports it. Blocks, in cascade order:

| Block                        | What it sets                                                   |
|------------------------------|----------------------------------------------------------------|
| `.operator`                  | Base operator tokens (v2). Mono font, 16px inputs under 768px. |
| `.operator.ink`              | Paper and ink palette, light and dark. Geist + Geist Mono. Utilities below. |
| `.operator.ink.v1`           | Density axis (`data-density`), sticky status line, marker highlight, radius frozen at 0.25rem, sticky bottom bar, ledger column var, metric grid. |
| `.operator.ink[data-accent]` | Landing only. Swaps `--primary` / `--primary-foreground`.      |
| "Ink type hierarchy"         | `.op-display` … `.op-label`, section rhythm.                    |

Palette (light):

| Token                | Value                        | Use                          |
|----------------------|------------------------------|------------------------------|
| `--background`       | `oklch(0.975 0.004 95)`      | paper                        |
| `--foreground`       | `oklch(0.13 0 0)`            | ink, and every border        |
| `--muted`            | `oklch(0.94 0.005 95)`       | section tone, hover, sampled band |
| `--muted-foreground` | `oklch(0.45 0 0)`            | secondary text, idle glyphs  |
| `--op-inset`         | `oklch(0.99 0.003 95)`       | log panes, code blocks       |
| `--op-rule-soft`     | 16% ink                      | row dividers                 |
| `--primary`          | ink; landing accent `signal` `oklch(0.64 0.21 32)` | filled buttons |
| `--ring`             | `oklch(0.59 0.2032 256.82)`  | focus only                   |
| `--success/warning/destructive` | from the base theme | status glyphs only           |

Utilities:

| Class            | Purpose                                                        |
|------------------|----------------------------------------------------------------|
| `.op-label`      | 10–11px uppercase tracked label. Eyebrows, column headers.     |
| `.op-prose`      | Wrapping body copy in the sans face.                           |
| `.op-rows`       | Children separated by soft rules.                              |
| `.op-row`        | A row of height `--row-h` (density aware).                     |
| `.op-raise`      | The one raised element. Hard 3px shadow, ink border.           |
| `.op-primary`    | Primary button: 2px hard shadow, translates on press.          |
| `.op-inset`      | Inset pane background.                                         |
| `.op-status`     | Status line / attention panel link styling (underline soft, ink on hover). |
| `ShellSlotsProvider` | Shell-provided DOM slots (`crumb`, `attention`) that PageTitle and StatusLine portal into. |
| `.op-sticky`     | Sticky under the header. `.op-sticky-bottom` for the save bar. |
| `.op-fill`       | Filled with `--primary`. Landing closing CTA only.             |
| `.op-fill-ink`   | Ink fill (selected option, icon send). Hover mixes 15% paper in, press nudges 1px. |
| `.op-fill-destructive` | Red fill for "run it" / "delete". Hover darkens 14%, press nudges 1px. Never hand-roll `hover:bg-destructive`: it is a no-op. |
| `.op-pressed`    | Momentary pressed look, used when ⌘S clicks the save button.   |
| `.op-cols`       | Ledger row: `grid-template-columns: var(--cols)` from md up.   |
| `.op-metric-grid`| Metric tiles: grid draws dividers, tiles stay plain.           |
| `.op-section`    | Landing sections only. `data-tier` major/minor, `data-tone` muted. Not for console pages. |
| `.op-block`      | `Section` on a console page: title 600 + one body. `.op-block + .op-block` draws the ink rule with 1.25rem above and below. |
| `ProjectMark`    | A project's favicon/logo at 16px in rows, lists, palette and breadcrumb, 24px beside a page title; monogram fallback (first letter, ink on paper). Served from the console's own origin, never hot-linked. |
| `.op-grid`       | Put on a grid of Sections that sit side by side (four breakdowns): they are peers, so the sibling rule is suppressed. |
| `.op-kv` / `.op-timeline` | `KeyValue` / `Timeline` bodies: framed (ink border), `> * + *` draws a soft rule between rows. |
| `.op-halves`     | `Columns`: main column + 18rem aside at xl, full page width (brand §6 "edges align"); below xl the aside stacks behind an ink rule. |

Type scale (weight is the signal; see `brand-guidelines.md` §2):

| Class         | Weight | Use                                                      |
|---------------|--------|----------------------------------------------------------|
| `.op-display` | 800    | Landing hero only. Never in the console.                 |
| `.op-h1`      | 700    | Landing major section title.                             |
| `.op-h2`      | 600    | Minor section or panel title. Largest tier in the console.|
| `.op-h3`      | 600    | Item title in a grid, settings section title.            |
| `.op-lead`    | 400    | Sentence under a title, muted.                           |
| `.op-label`   | 500    | Eyebrow, column header.                                  |

Frozen decisions. Do not reopen without a written reason:

- Geist and Geist Mono. Radius 0.25rem. 1px ink borders. 8px spacing grid.
- No accent axis in the console. Landing accent is `signal` on the primary CTA.
- Density default is comfortable, `d` toggles, choice persisted.
- Motion is 100ms, transform / shadow / colour only. No entrance animation.
- Charts are linear lines, ink on paper, no fills, no animation.

## 5. Status vocabulary

`src/components/op/status.tsx`. Five states, one glyph each, one colour each.

| State     | Glyph | Colour  | Meaning                                              |
|-----------|-------|---------|------------------------------------------------------|
| `ok`      | ●     | success | healthy, passing, deployed                           |
| `warn`    | ◐     | warning | degraded, above threshold, expiring                  |
| `error`   | ×     | destructive | failing, unreachable                             |
| `idle`    | ○     | muted   | not deployed, not configured, nothing yet            |
| `sampled` | ◌     | muted   | telemetry head-sampled past the plan allowance       |

`sampled` exists because the pricing page promises that past the allowance
"telemetry is head-sampled and the console says so; it is never silently
dropped". That promise is a UI contract. It shows in the status line, as a band
on the chart, in the chart footer and in project settings. `STATE_RANK` orders
lists by attention. `worst(states)` picks the status line glyph.

## 6. Components

All in the `@temps-sdk/op` package (`web/packages/op`), imported through the
one-line re-export at `src/components/op/index.ts`. Reference page with
every state: `/op-components`. These are what a new screen reaches for first;
shadcn primitives are for what these do not cover.

### StatusLine, Phrase, Status

The page's verdict. Inside the console shell it does not take a line of the
page: `StatusLine` portals into the header's attention slot and renders as a
glyph + count (`× 2 · ◐ 1`, `sampled` counts as a warning). A page with
nothing wrong shows one quiet green glyph and no number. Clicking it opens
the list on demand: the verdict sentence first, then every `more.items`
entry, each with its own `Phrase`. Escape or a click outside closes it. The
same API as before, so every screen kept working when the line left the
page; the shell provides the slot through `ShellSlotsProvider`.

Outside a shell (docs, demos, a page that has no header) the inline form
renders: one glyph (the worst state on the page), one sentence under ~60
characters, at most one `Phrase`, and `more` as a muted link on the right
that unfolds the items in place. Counts, facts and "fine" things never
appear in a verdict, in either form.

Wrong: `◐ 6 projects · × billing-worker failing · ◐ api-gateway 0.61% · 4 deploys today · cert 6d`.
Still wrong: `× billing-worker is failing health checks. api-gateway error rate 0.61% since dep_91a.` + muted tail.
Right: `× billing-worker is failing health checks.` with `+1 warning` on the right.

### Num, Metric, MetricGrid

Mono tabular numbers, unit after the value in muted, en dash for nothing, zero
is "0". `Metric.baseline` is required: every delta names what it is compared to
("since dep_91a", "vs yesterday", "90d window").

### Callout

An alert inside a page: `state`, a `title` in the state colour with the
glyph, an optional `quote` (the other system's message, verbatim, in mono),
the consequence sentence as children, and an `action`. A 2px left rule in the
state colour and no box: the rule is the alert, and a frame would sit inside
the page's other frames. The quote sits on the inset tone, not in a border.
`role=alert` when error. StatusLine is the one-sentence verdict
that rolls up into the header; Callout is the evidence block where the fault
applies (an expired git connection above its ledger, a missed backup above
the backup line). Never render one when nothing is wrong.

### PageState

One component, four states: `loading` (skeleton rows, never a spinner),
`empty` (title, reason, next step), `unconfigured` (what is missing, an example
of what the surface will show, a link to the settings page), `error` (message,
resource, retry). Nothing renders blank. Replaces the console's three empty
state components and spinner-as-page-state.

### Kbd

Platform-aware key badge. `'⌘'` becomes Ctrl off macOS. Always an accelerator,
never the only entry point.

### EchoDialog

Every destructive or irreversible action. Title and a description that says
what is lost and what is kept; typed confirmation of the resource name, with
the name in a mono badge that is itself the copy button (clicking the name or the icon copies it) right before the input; step progress
mirroring the backend. The destructive button is a filled red only once the
name matches; before that it is a red outline at reduced opacity, never a pale
fill with white text. `echo` (the CLI equivalent) is accepted and documented,
not rendered. Rollback and delete share it. There is no other confirm dialog.

### Picker

The searchable select. Anything with more than about seven options, or options
the operator recognises rather than recalls (branches, images, regions,
environments, providers), is a Picker, never a plain `<select>`. Mono trigger
the height of an Input showing the current value; opens to an autofocused
filter box and grouped rows (`group`), each with a state glyph slot, a fixed
16px slot for the option's kind `icon` in muted ink, the label, and a muted
`meta` on the right (last commit and age, region, "1 deploy ahead"). The
current value is marked ● in the glyph slot. The two slots are separate and
stay separate: `icon` says what the option is (a worktree, a sandbox, a
permission mode), the glyph says how it is, and an icon is never tinted by
`state`. `icon` is required wherever the options are of different kinds — the
workspace picker (worktree · shared main checkout · sandbox), a list mixing
environments and regions — and left off when every option is the same kind. `allowCustom="use branch"` offers the typed text
as a row for values not in the list. `loading` and `error` are states inside
the list, not a spinner on the trigger: they say what was being fetched and
from where, quote the source's error, and offer retry. Reference: branch
picker in project settings (`/v1?p=api-gateway`, settings tab) and
`/op-components#picker`. The real console's `SearchableSelect` in
`web/src/components/ui/searchable-select.tsx` is the migration target.

### Command palette (`⌘K`)

`CommandDialog` from `src/components/ui/command.tsx`, skinned: the magnifier is
replaced by a `>` prompt, the whole dialog is mono, group headings are
`.op-label`-style uppercase, the selected row is an ink fill, and there is no
shadow — the ink border is the elevation. It is anchored near the top, not
centred, so the list does not grow its tail out of the viewport. `⌘K` opens it
everywhere and a visible **find** button in the header opens it too; the key is
the accelerator, never the only entry point.

Every row leads with a fixed 16px slot, and the palette is the list where the
kind icon matters most, because it is the one list that mixes every kind the
console has. *Projects* rows are the state glyph, then the project's identity
mark and its kind (app · worker · static), then the name. *Pages* and
*resources* rows carry the same icon the sidebar gives that page, so the
palette and the nav read as one map: databases is `Database`, traces is
`Waypoints`, uptime is `Globe`, git providers is `GitBranch`. *Commands* rows
carry the icon of what the command does (`Rocket` deploy, `HardDrive` back up).
Bare words in a palette group are a bug: a reader scanning results has nothing
but the word to tell a page from a project from a command. Icons are muted ink;
the state glyph keeps its own slot beside them. Reference: `/components#palette`
and the `⌘K` palette on `/v1`.

### Switch and Toggle

Under the ink skin a switch is a square track with a 1px ink border: off is
paper with a muted thumb, on is an ink fill with a paper thumb (globals.css,
`button[role='switch']`). The stock shadcn pill filled the track with `--input`
when off, which is ink here, so off looked on. In forms, pair the switch with
the word: `on` / `off` in mono next to it, and disable the fields the switch
governs when it is off, with help text that says so.

### SecretValue

A variable value in a row. Plain values are mono with a copy button. Secrets
are dots until the eye reveals them; copy always copies the real value, so a
secret can be pasted without being shown. Reveal is per row; the variables tab
also has a page-level "show values". In the real console a reveal is an API
call and must be audit-logged, which is why it stays an explicit click.

### TimeChart, RangePicker, ChartFooter

`thresholds={[{ y, label, state }]}` draws dashed horizontal reference lines
labelled at the right edge in the state tone (a vital's good / poor line).

`RangePicker` takes `custom={{ from, to, onChange }}` to add a last button
that opens two datetime fields under the strip (from, to, a retention note,
cancel, apply; "to" must be after "from" and the form says so). Once applied
the button reads the window in mono ("Sep 5, 10:00 → Sep 6, 11:00") and
`value` is `custom`, so the page's meta and the sparkline column can name
the window too. Errors uses it; analytics, proxy and metrics should.

Every time axis carries deploy markers (linked both ways to deploy rows through
`hot` / `onHot`), the sampled window if any, and the retention horizon in the
footer. Ranges beyond retention are struck through, not hidden, and `onGated`
lets the page say which plan keeps that range. Readout above the plot for touch.

Deploys land in bursts. Markers whose labels would overlap at the current
width (about 72px) collapse into one label, "3 deploys", while every deploy
keeps its own dotted line, so the axis never overprints and never undercounts.
Clicking the label opens a strip under the plot listing the members with tag,
time and commit note; hovering a member lights its line, clicking calls
`onOpen(id)`. Markers accept `at` and `note` for this.

Selecting time: with `onSelect` (or a controlled `selection`), dragging across
the plot selects a fraction of the axis. The band is ink at 6% with a dashed
edge; a strip under the plot states the bounds, the point count, and "clear
(esc)". The page narrows whatever sits under the chart to the window: the
Email page's ledger shows only mail sent in the selected hours and its footer
says so. A click without a drag clears; the selection never changes the
chart's own range (that is the RangePicker), it filters what is below.

### Sparkline, LogViewer, EmptyPlaceholder

In `src/components/ui/`. Sparkline for ledger cells. LogViewer has a gutter,
level colour, `/` search, n/N, follow toggle. EmptyPlaceholder is the older
onboarding component; PageState `unconfigured` supersedes it for new work.

## 7. The three page templates

`src/components/op/templates.tsx`. Every console screen is one of these. A
screen that does not fit is a reason to extend a template, not to start from a
blank div.

All three take `title` and `meta` and render a `PageTitle` first. It carries
its own top padding (`pt-5`) so the first thing under the shell header has
air, then the screen's name in `.op-title` (the one 700-weight line
on a console screen) and one or two mono facts that place it
(`production · dep_91a · main`, `sbx_9f3 · temps/sandbox:node22 · fsn1`).

The trail lives in the shell header: nav group, then the list page as a
link when on a detail, then the current page. The shell renders the
ancestors and exposes a slot; `PageTitle` portals its own title into it as
the last crumb, so a detail page's trail ends in the resource's real name,
never its id, and the page never assembles its own path. Outside a shell a
`crumbs` prop renders the trail above the title. Screens that are not a
template use `PageTitle` directly.

The list is one CSS grid from md up and every row is a subgrid, so column
widths are computed across all rows. Track vocabulary for `grid`: `Nfr` for
the long text columns (name, message: they truncate), `NNpx` only for numbers
of known width, and `minmax(NNpx, max-content)` for short text of varying
length (cadence, source, a state word) so it grows to the widest row instead
of truncating "sundays 04:00" in a fixed 90px. `Nfr` is rewritten to
`minmax(6rem, Nfr)` so one unbreakable 90-character cell truncates instead of
widening its column; when the fixed tracks still exceed the container (a
ledger in a narrow column) the rows scroll sideways rather than breaking the
page.

Columns are labels or `{ label, key, numeric }`. A column with a `key` is
sortable: clicking its header cycles ascending → descending → off, where off
is the ledger's default order (the `hint`, usually attention first). One sort
at a time, the filter box is for narrowing. Rows carry raw values in `sort`
so "4.2 GB" and "3d ago" sort as numbers; empty values sort last in either
direction. The footer names the active sort and offers "clear". Sort can be
controlled (`sort`/`onSort`) for URL persistence, or left internal.

Pagination, one way everywhere: pass `page` (`{ page, pageSize, total, onPage,
onPageSize? }`) and the footer becomes the pager, `1–20 of 1,284 · ‹ prev ·
next › · page 1 of 65 · 20 per page`. It matches the API (page-numbered,
server-side, default 20, max 100, sizes 20 / 50 / 100 via `PAGE_SIZES`), so
`rows` are the current page and `total` in `page` is the filtered count from
the server. Prev and next are the only moves, never a row of numbered
buttons: the filter and the sort are for finding a row, paging is for reading
in order, and an operator has to be able to say "page 3" to a colleague,
which rules out infinite scroll. `[` and `]` page from the keyboard and reset
the cursor to the first row. Filtering, sorting or changing the time window
resets to page 1; the caller does that in its handlers. `Pager` is exported
for lists that are not a Ledger (event timelines, audit rows) and renders the
same line. Where the total is unknown (a cursor API), show `1–20` with next
only and say "more" instead of the total; do not invent a count.

**Ledger**: title, status line, filter with `/`, actions, rows with `j` `k` `⏎`,
footer with counts and keys. Rows sort attention first. `grid` is the CSS
`grid-template-columns` for md and up; phones get name + note + glyph. Pass a
`PageState` as `state` to replace the rows. A row takes an `icon`: the kind of
record it is (app / worker / static project, database engine, control plane /
worker node, span kind), drawn in a fixed 16px slot at the head of the first
cell and before the name on a phone, in muted ink. It rides the first cell
rather than taking a column of its own, so no `grid` string changes and no
single-kind ledger carries an empty slot. It is required when the list mixes
kinds and left off when the ledger's title already names the kind (the deploys
of one project). The state glyph stays where it is; an icon never carries a
state colour. Used for projects, databases,
errors; intended for deploys, domains, users, backups, sandboxes, email.

**The observe primitives** (`src/components/op/viz.tsx`, demos on
`/op-components#breakdown`). SVG and CSS only, no library. All obey the row
rules: ink on paper, mono tabular numbers, colour only through the five
states, soft rule between rows, ink frame around the group.

| Primitive | Shape it draws | Where web needs it |
|---|---|---|
| `Breakdown` | one dimension ranked: label · count · share, share as an ink bar behind the row; `icon` in a fixed 16px slot (flag, browser mark, channel, device), muted ink, required when the rows are of different kinds and omitted when they are not; fills its Section's height with the footer pinned to the bottom so grid peers align; rows with `children` open in place with a path header; honest "other" remainder | the ten analytics cards, dimension lists, page/event detail |
| `GeoMap` | countries filled by state (ok / warn / error tones, muted for no data); on a fine pointer the hovered country reads at the pointer and nothing sits under the map, click opens; below md the readout is a row under the map, tap to read and tap again to open; second view of a by-country list, never the only one | speed by country, locations |
| `Sparkline` | one ink line in a cell, last point marked, no axes, never its own number | page rows, observe tiles, metric lists |
| `Funnel` | bars by share of entrants; conversion and drop-off per step, drop-off ≥ 50% red | funnels |
| `Flow` | "from → to" ranked with count and share; entries/exits are the same rows with one side empty | journey |
| `StatusStrip` | one segment per bucket coloured by state, hover reads checks/p50/p95 | monitors, monitor detail |
| `ScoreRing` | 0–100 arc, number in the middle, state at Web Vitals thresholds | speed insights |
| `CalendarHeatmap` | days × weeks in five ink intensities | deployment activity |
| `Live` | "● live · every 30s", pausable | any polling surface |
| `Waterfall` | collapsible span tree, bars by offset/width, error spans red | trace detail |
| `StackTrace` | frames most-recent first, in-app open with source context, vendor muted | error event detail |
| `LogLines` | time · level glyph · source · message; level toggles; hidden count said | runtime logs, build logs |
| `Stages` | build steps with state and duration; the running one streams `LogLines`; one open at a time | deployment detail |
| `Histogram` | distribution with avg · p50 · p90 · p95 · p99 selector; chosen value is a red rule, tail past it muted | metrics explorer |

Not built, by decision (`console-inventory.md`): the WebGL globe (dropped), the
choropleth (optional view on the locations dimension, later), the rrweb
player and Monaco data browser (embedded tools; the shell goes around them).

**The record page, deterministically.** A single record (an email, a run, a
finding, a deploy) is built from these primitives and nothing else, so the
next one looks like the last one:

```
<Detail title meta mark? status actions
        lede={<Lede state="ok" word="delivered">10h ago · to x@y · via ses-eu</Lede>}>
  <Columns>                              main column + 18rem aside at xl; stacked with a rule below
    <div>                                main: the thing, then what happened to it
      <Section title="Content" action={<Segmented html|text/>}>…framed content…</Section>
      <Section title="Events" meta="3 · last 10h ago"><Timeline items /></Section>
    </div>
    <div>                                aside: reference facts, small
      <Section title="Headers" meta="8"><KeyValue rows compact /></Section>
    </div>
  </Columns>
</Detail>
```

Tiers, and nothing at any other size or weight: title 700/20 · Lede 600/18 with
glyph · Section title 600/14 · row event word 500 · rest 400 muted. `status`
becomes the header's attention count inside the shell; `lede` is the page's own
answer line, the one `.op-raise` on the page, and is required for a record
page. Title-row actions sit right of the title when `lede` is given. Nothing on the page is capped narrower than the page (brand §6 Taste).
`KeyValue` and `Timeline` are framed groups (ink border, soft rules between
rows). `Timeline` items carry an `icon` that names the event kind; the page
owns the vocabulary (`MAIL_EVENT_ICONS`) so the same event is always the same
icon; `state` only colours failure red or not-real muted. Two faithful
renderings of the same content (html/text) are a 2-view `Segmented` in the
section's `action`, never a collapsed section or tabs. A fact appears once:
not in the title meta and again in a section meta.
```

`Section` = `SectionTitle` + one body; sections in a column separate with an
ink rule and 1.25rem above and below through a CSS sibling rule, so the first
never has one and nobody passes a flag. `KeyValue` is the grouped list of
facts: soft rule between rows, key left muted at 11rem, value right in ink,
mono unless `mono: false`, optional state glyph. `Timeline` is what happened
in order: time mono at 3.5rem, event as glyph + word at 500, note muted.
Order of sections is fixed: what happened → the facts → the content; actions
are in the title's `ActionBar`, never inside a section. If a record needs a
fourth kind of body, add a primitive here first.

**Sections inside a page have a title.** `SectionTitle`: `.op-h3` (1rem,
600) with the count or one fact in mono beside it and an optional action on
the right. That is the tier between the page title (700) and row text (400).
The email page was three sections headed by 10px uppercase eyebrows and read
as one grey column with no way in; now "What happened · 3 events · last 14m
ago", "Headers · 8 fields", "What was sent · from → to". `.op-label` stays
for column headers, field names, eyebrows and key badges, never for the
title of a section. Inside a section, the word that carries the state (the
event label, the row's status) is 500; everything explanatory is 400 muted.

**Layouts by data and operation.** Before choosing tabs, ask what the data is
and what the reader does with it. The layout follows from the pair; tabs are
the last resort, not the default.

| Data | The reader… | Layout |
|---|---|---|
| Many records of one kind (projects, traces, sent mail) | scans, filters, sorts, opens one | `Ledger`: one grid, filter, sort, pager. Never cards. One per screen: it owns `/`, `j` `k` `⏎` and the footer. |
| Two kinds of record on one page (domains and providers) | works with one kind at a time | Two facets: a tab each. Never two Ledgers stacked. A secondary list that must share the page is a framed list of ≤5 rows with a link to its facet, no filter/footer/keys. |
| One record that fits a screen (an email, a backup run, a scan finding, a deploy) | reads it top to bottom, acts once (resend, restore, roll back) | one page in reading order, no tabs: what happened → what it is → the facts. Two columns at xl when the halves are independent (timeline left, content right). Secondary text behind `<details>`, never a tab. |
| One resource with several facets, each its own list or form (a project: deploys, environments, variables, settings) | comes for one facet at a time | `Detail` with tabs, one row, 2–6 |
| Time series | asks "when", "since which deploy" | `TimeChart` with markers and drag selection; the list under it follows the selection |
| A few numbers with a baseline | glances, compares to before | `MetricGrid`; every delta names its baseline |
| A configuration | edits, saves once | `Settings` with sections and a sticky save bar; side nav at @3xl |
| Records to compare across a second axis (variables × environments) | looks for the gap | the matrix view, chosen from the scope Picker, cells are toggles |
| Nothing yet, not set up, failed to load | needs to know why and what next | `PageState` |

The test for tabs: does the reader come for one facet and ignore the others?
A project, yes. An email, no: they open it because something went wrong and
need the events, the content and the headers together to see what. Hiding
two of the three behind clicks makes them look for it. The email page was
three tabs and is now one page: events and headers on the left, the rendered
message on the right, the text version under a `<details>`.

**One axis per control.** A page gets one row of tabs, ever, and it answers
one question: which facet of this resource (overview, deploys, variables,
settings). Every further axis inside a tab is not another row of tabs; the
control says what kind of axis it is:

| The axis is… | Control | Where | Example |
|---|---|---|---|
| a facet of the resource, 2–6 of them | Tabs (`Detail`) | under the title | overview · deploys · variables · settings |
| a scope: which instance the list is about, any number of them | `Picker` | first thing in the toolbar, read as a sentence: "in production" | variables in [production ▾]; traces for [api-gateway ▾]; metrics of [db-main ▾] |
| 2–4 views of the same list, mutually exclusive | `Segmented` | in the toolbar, after the filter | all · errors · slow; list · matrix |
| sections of a form, many of them | side nav (`Settings` at @3xl) | left of the form | general · build · domains · danger |
| time | `RangePicker` + chart selection | title actions / the chart | 24h · 7d · 30d |

Tabs inside tabs never happen; the variables tab had one (six page tabs, then
environments as a second tab row) and now says "in production" with a Picker,
whose options carry the variable count and state per environment and a
"compare" group for the matrix. The test for a Picker over tabs: would a
seventh value break the layout? Environments, services, branches and projects
all grow; facets do not.

**Detail**: title, status line, tabs with number keys, actions on the right, body.
Body convention: one TimeChart, one MetricGrid, one `.op-raise` (the incident
or activity thread), deploy rows linked to the markers. `Segmented` for compare
and range choices.

**Settings**: title, status line, sections with a side index, sticky save bar that
⌘S clicks (so pressed and disabled states are honest), danger zone whose only
action is an EchoDialog. `Field` lays out label, control, help; it goes to
one row via a container query (`@md:`) on the section body, so it stacks when
the section is narrow regardless of viewport. The side index and the two-column
layout are also container-queried (`@3xl:`), so Settings works inside a 360px
box as well as on a page. Metric tiles show the state glyph before the baseline
for every non-ok state, so a `sampled` tile is visibly sampled.

### Record page checklist (enforced)

The email record shipped with a Lede that had no facts, a meta that was
only the id, and a verdict that repeated the Lede word. That is three rules
from §3 of the brand and this section, and nothing caught it. Now something
does. Before a record page is done:

1. **Meta places the record**: id · project · environment (and the one
   fact that names it, like "to" for a mail). Never the id alone.
2. **The verdict says what to do**, or "Nothing to do: …" with the fact that
   proves it. It never repeats the Lede word ("Delivered 3h ago" under a
   Lede that says "delivered").
3. **The Lede carries four to six facts** in `facts`: the values the reader
   wants without scrolling. A Lede with only a sentence is a headline, not
   a lede.
4. **A fact appears once.** What is in the meta or the Lede is not a row in
   the aside. The aside is what is left after the meta and the Lede: the
   reference the reader did not come for, and nothing they have already
   read. There are no exemptions for facts that feel like configuration.
5. **Content, then what happened, then reference**: main column is the thing
   itself and its timeline; the aside is KeyValue and lists of at most five.
6. **No tabs on a single record** unless a facet is its own list or tool.
7. **Actions do, facets go.** Nothing in the actions row may only switch
   to a tab; the tab row is the way to a facet.
8. **A drawn control is a wired control.** A filter box, a Segmented, a link
   or a button that cannot change anything does not ship: wire it, make it
   plain text, or remove it. `href="#"` with a `preventDefault` is never one
   of the three. Action props carry a typed destination -- the pattern is
   `PageState.settingsHref: /${string}`, so a dead link fails the build
   instead of the reader.

Enforcement, so it does not happen again:

- **In the browser (dev):** `Lede` warns when it has fewer than three
  facts; `Detail` warns when it has a lede but no meta or no status. The
  warnings name the rule and the handoff section.
- **In lint and CI:** `bun run lint` runs `scripts/audit-records.mjs`, which
  fails on a `<Lede>` without `facts=`, a `<Detail lede=…>` without `meta=`,
  a KeyValue row keyed `project · environment`, `message id` or `id`
  (facts that belong in the meta or the Lede), a literal word that appears
  in both a `meta=` and the same file's `facts=` (rule 4, a fact said
  twice), and a `<Detail status=…>` whose `<Columns>` has no `lede=`
  (rule 3, a record page with no lede). The last two are heuristic and
  literal-only: a fact assembled from an expression is invisible to them,
  so the browser warnings and the review still matter. Run it alone with
  `bun run audit:records`.

## 7b. Redesigned surfaces on the templates

`src/sections/ConsoleV1Env.tsx` rebuilds three existing console surfaces on
v1, using the real API shapes from `web/src/api/client/types.gen.ts`
(`EnvironmentResponse`, `DeploymentResponse`, `EnvironmentVariableResponse`).
They answer user feedback recorded on 2026-09-04:

- **Promote was hidden** in a per-row dropdown on the deployments list. Now:
  the environments tab draws the promotion path (staging → production) with
  promote as its primary button; every deploy that is ahead of production has
  a visible "promote to production" action in its row; the status line on both
  tabs says when something is promotable. One `PromoteDialog` (an EchoDialog:
  `temps deploy promote <tag> --to <env>`) serves all three entry points.
- **The variables page mixed environments.** The old header dropdown labelled
  "Preview values for" only changed linked-service preview values, while the
  list stayed global with a pill per environment, so choosing staging still
  showed production-only variables. Now each environment is its own view that
  shows exactly what it receives, with an "also in" / "only here" column. The
  "matrix" view is the single cross-environment view: one column per
  environment, each cell a toggle, preview inheritance shown as "✓ preview".
- **Bulk association.** Select with `x` or the checkbox, `⇧A` for all, then the
  sticky bulk bar. Inside an environment view the selection is by definition
  in that environment, so the bar asks only two things: "also add to
  <other env>" (disabled with "already in" when nothing is missing) and
  "remove from <this env>". Never offer the environment the reader is
  standing in as a target. The matrix view, the cross-environment view, shows
  one control per environment stating where the selection *is*: checked (all
  in), dash with `7/10` (some), empty (none); clicking completes the set. Each
  goes through an EchoDialog with the count spelled out. There are no
  "add to" / "remove from" rows of identical environment names: they read as
  the same buttons twice. Delete stays on the right. The status line's
  "3 variables exist in staging but not production" selects those three and
  opens the matrix.
- **Search** on `/`, key only, never values.

**Sandboxes, traces, metrics** (`src/sections/ConsoleV1Observe.tsx`), using
`SandboxInner`, `SandboxEvent`, `SandboxStatusResponse`, `TraceSummary`,
`SpanRecord`, `SpanStats`, `MetricBucket`: On traces, the latency chart is the time filter: drag across it and the trace ledger narrows to traces whose start falls in the selected half-hours, the footer saying so; the verdict's "since dep_91a" link selects that window for you.

- Sandboxes ledger (`?p=sandboxes`): status, lifecycle, runtime, resources,
  source repo, linked agent run. Host capability line underneath (docker,
  firecracker, image ready) so "why can't I create one" is answered on the page.
- Sandbox detail (`?p=sandbox:<id>`): Detail template. Metric grid for cpu,
  memory, disk, uptime with the limit as baseline; identity rows; inset
  terminal; the agent run as the one raised element with changed files and
  next actions (open PR, deploy to staging). Sleep / wake / destroy, destroy
  through EchoDialog. A failed sandbox renders PageState error with the pull
  error and a retry.
- Traces (`?p=traces`): latency chart with p95 and p50 and the same deploy
  markers as the project chart; ledger with a duration bar per row; filter
  all / errors / slower than p95. Second tab "operations" is SpanStats per span
  name with tail ratio (p99 ÷ p50) flagged above 10.
- Trace detail (`?p=trace:<id>`): waterfall with depth, kind, exception event
  ticks, `j`/`k`; the selected span as the raised element with attributes and
  events; links out to the error, the replay and the deploy. The status line
  says where it failed and where the time went.
- Metrics (`?p=metrics`): metric list with kind glyph and alert marker;
  histogram aggregate switch (p95 / p50 / avg / max); chart with deploy
  markers and the alert threshold in the footer; tiles that compare to before
  the last deploy; breakdown by the metric's first attribute.

Real-console mapping: `components/project/ProjectDeployments.tsx`,
`components/project/settings/EnvironmentVariablesSettings.tsx` (1,734 lines),
`pages/EnvironmentsTabsView.tsx`, `pages/Sandboxes.tsx`, `pages/SandboxDetail.tsx`,
`pages/TracesList.tsx`, `pages/TraceDetail.tsx`, `pages/MetricsExplorer.tsx`. The CLI verbs used in the echoes
(`deploy promote`, `env attach/detach/unset`) are proposals; check
`apps/temps-cli` for the real names before wiring.

### Backups (`/v1?p=backups`)

One screen, three tabs: schedules, backups, sources. The live console shows
only the S3 sources table and hides overdue schedules behind a header bell.
Here the verdict is first: which job failed on which service after how many
attempts, then the overdue schedule as "+1 warning". A running backup shows
its engine step (`upload_parts`) and live size in the row. The failed job is
a `Callout` above the ledger, not a raised panel: the × and its title in the
error tone, the 2px left rule, what the engine said quoted verbatim on the
inset tone, one sentence of what it costs, and the fix as the action. A
fault is never a box, and the one raise on a backups screen is not spent on
it. Sources carry
"make default" as an EchoDialog. Retention is per schedule; PITR is per plan
and said so in the footer.

### Git providers (`/v1?p=git`, `git:<id>`)

Every provider row and the provider page title start with the provider's
mark (`GitProviderLogo`: GitHub, GitLab, Gitea, Bitbucket; `github_app`
shares the GitHub mark; anything else is a branch icon). The paths are the
ones `web/src/components/git/ProviderLogo.tsx` uses, drawn in `currentColor`
instead of brand colours: on a console surface colour means status, so the
GitLab mark is not orange here. Muted in rows, ink in the title, 16px and
20px.

Providers ledger leads with connection health, not provider type. The detail
screen's status line is the expired installation with Reconnect as the only
link; the raised panel quotes what GitHub said and what has not deployed since.
No connection yet is PageState unconfigured ("Installation required") with an
example of a connected account. Settings: default provider, auto-deploy new
repos, webhook endpoint and secret rotation (EchoDialog), delete.

### Security (`/v1?p=security`, `scan:<id>`)

Scans, headers and access rules under one title. Each environment's last scan
is a row with critical/high/medium/low as sortable numeric columns; the
status line names the worst finding and links to it. A failed scan is an
error row, not a missing row. Scan detail: findings ledger with installed →
fixed and an all/fixable toggle; the status line names the package and the
fix ("rebuild"). Headers tab is a Settings form over SecurityHeadersSettings
with a preset Picker that flips to custom on edit. Access tab covers rate
limiting, attack mode (off/challenge/block), allow-list, password protection
and geo restrictions; "block all" is the danger action.

### Errors (`/v1?p=errors`, `issue:<id>`)

`src/sections/ConsoleV1Errors.tsx`. The Sentry shape with the noise removed;
what a phone and a desktop both need is the same eight things, so the row
and the record are built from those and nothing else.

- **Issues ledger.** One row per issue: type in medium weight and message in
  muted on the first line; on the second, the fact that matters for its
  state ("regressed in dep_91a · fixed in dep_88c", "new in dep_91a",
  assignee) and the culprit file:line. Then project with its mark, a 24h
  sparkline, events, users, last seen, first seen. State is a `Status`, glyph and
  word together: × regressed / new / unhandled, ◐ handled, ● resolved,
  ○ ignored. The glyph never travels without its word and no other value in
  the row takes a tone; a legend under the ledger would not buy that back.
  No level badges, no coloured pills, no avatars. The phone row keeps type, message,
  culprit with the project mark, and "events · users · last" on one line.
- **Range** is a RangePicker in the action slot (1h · 24h · 7d · 30d · 90d ·
  custom window); the meta and the sparkline column carry the chosen range.
- **Status filter** is a Segmented in the action slot: for review (regressed
  and new), unresolved, regressed, resolved, ignored, all. It defaults to
  "for review" because that is the inbox. The meta counts both.
- **Verdict** names the worst issue as a sentence with the release that
  broke it and the one that had fixed it; a second line for the new one.
- **Issue record.** Title is type + message, meta is id · project · env.
  Verdict explains the regression in words (what the endpoint now does).
  Lede "regressed" with events 24h, users, first seen and the release that
  fixed it, last seen, the release it came back in (×), handled (◐ if not).
  Actions: resolve (primary), ignore (typed, because it silences), assign,
  open in editor. Content: the events chart with users as the thin line
  and the deploy marker; the stack trace with in-app frames open and source
  mapped; breadcrumbs as a Timeline with icons per kind (navigation,
  request, click, console, the exception) where the request that returned
  204 carries ◐ so the cause reads in the trail. Aside: latest event as a
  compact KeyValue with a replay link, three top tags as three-row
  Breakdowns, similar issues. Facets: events (a paged Ledger of
  occurrences) and tags (a grid of Breakdowns).
- **States.** Fresh: unconfigured PageState with the two-line SDK init and
  the DSN link. `?fail=1`: the error store itself is down; the page names
  the resource and retries.

### Settings (`/v1?p=settings`, `settings:<slug>`)

`src/sections/ConsoleV1Settings.tsx`. The live sidebar has twenty entries
under General / Access / Infrastructure / Security, organised by which
slice of the settings row a page writes: rate limiting is rendered on two
pages, IP rules on two, "monitoring" is three pages that do different
things, Version and Plugins and Worker Nodes are status pages in a settings
tree, and Let's Encrypt lives on a page called Platform. The redesign is by
what the operator is doing.

**Five groups, fifteen pages:**

| group | why | pages | was |
|---|---|---|---|
| instance | set at install, rarely again | Domain & TLS · Updates · Builds & registry · Timeouts | Platform, Version, Build Limits + Docker Registry, Request Timeouts |
| access | who can do what | Users · Teams · Sign-in · API keys | Users, Teams, Authentication + the admin gate (was on Security Headers) |
| edge | what the proxy does to every request | Security headers · Traffic rules · Custom routes | Security Headers (headers only), Rate Limiting + IP access control (once), Load Balancer |
| data | where telemetry goes and how long it stays | Store · Retention · Alerts | Metrics Monitoring split in two, Disk Monitoring + Notifications |
| fleet | the machines and code this instance runs | Nodes · Cluster · Plugins | Worker Nodes (the table), Worker Nodes (token + how-to + Cluster DNS + Cluster trust), Plugins |

Things that are not instance settings stay where they are used: domains and
certificates, DNS, git and email providers, AI providers, backups, the
agent sandbox, skills and MCP servers, and every per-project setting. Alert
rules live with what they watch; the Alerts page is only where alerts are
sent, and says so.

**Rules the pages follow:**

- **The hub shows state, not descriptions.** Every row is a kind icon (the
  14px monochrome mark rule from brand §6), the page title and its current
  value in mono; when something is wrong the state glyph opens the value in
  its tone rather than sitting in a column that is empty everywhere else ("2 concurrent · no limits · registry off"),
  or, when something is wrong, the problem in the state tone. Nobody opens
  a page to find out whether it is set. The hub's verdict names the worst
  thing and counts the rest.
- **Every page has a verdict** (StatusLine) about its own state, in words:
  "Certificate renewals will fail: no contact email", "The console answers
  to any IP", "ci-deploy expires in 6 days; CI deploys stop when it does".
- **Every field says when it takes effect**: ● now · ○ next request · ◐
  restart, in the field help, with the legend at the foot of the page. A
  page with a change waiting for a restart opens with a warn Callout and
  the restart action; the hub row says "pending restart".
- **Status pages are honest about it.** Updates is "running" facts plus two
  settings; Nodes and Plugins are ledgers with the operational actions in
  the footer.
- **Destruction is explicit.** Retention says in its danger zone that
  saving a shorter value deletes data and that the save bar will ask for
  "delete". Sign-in's danger zone signs everyone out. Pages with nothing
  destructive say "Nothing destructive here" rather than hiding the zone.
- **Ledger pages are ledgers** (Users, Teams, API keys, Custom routes,
  Nodes, Plugins): the Ledger template with the page's verdict, not a form
  with a table inside it.

### Uptime monitor (`/v1?p=uptime`, `monitor:<id>`) and the public status page (`/status`)

`src/sections/ConsoleV1Analytics.tsx` (MonitorScreen) and
`src/sections/StatusPage.tsx`. The live monitor page is three cards (current
status, uptime, average response), a strip of green blocks with a
four-colour legend, and a "Configuration" card listing URL, type, project id,
environment id, created. On the record recipe:

- **Verdict** says what happened in words: down 30 minutes at 20:30 right
  after dep_91a, connection refused from all three regions, up again since
  dep_91b. Slow is its own sentence and says it is not down.
- **Lede** "up / slow / down / paused" with uptime for the chosen range, p50,
  p95 (◐ above 1s), last check with its status and time, incidents in 30d,
  and how it appears on the status page.
- **Content**: the check strip for the range, the response-time chart (p50
  thick, p95 thin, deploy marker, 1s threshold), incidents as a Timeline
  with a cause and a resolution per entry. **Aside**: Check (method,
  expectation, regions, the down rule), Alerts (who hears, on
  what), Status page (shown as, group, what slow shows as, the page URL),
  Danger. Project id and environment id are gone: they are not something a
  reader acts on. The URL and the check interval are gone from the aside
  too, for the other reason: they are already the meta and a Lede fact, and
  a fact appears once (§7 rule 4). Range is a Segmented in the actions; check now and pause
  are the other two.

The **public status page** is one column, read on a phone during an
incident: the project mark and name, a verdict in words as the one raised
block ("API is down; Checkout degraded" or "All systems operational") with
the time of the last update, then components grouped (Platform, Services)
each with its state word, 90-day uptime and a 90-day StatusStrip, then
incidents with their updates (time, the phase as a `Status` -- glyph and
word -- then the text) newest first with open ones on top, then subscribe (email, RSS, webhook). Same
glyph vocabulary as the console, state tones only, no console chrome,
"powered by temps" in the footer. It is per project: `?project=<slug>`.

### Proxy (`/v1?p=proxy`)

`src/sections/ConsoleV1Proxy.tsx`. The live page is four metric cards and
four multi-line charts (status class, destination, error rate, latency
percentiles), each with its own colour legend. Here it answers four
questions in order and on one chart:

- **Verdict** names the one incident in the range as a sentence: which
  upstream reset how many connections at what time, what share of requests
  got a 502, and that it has been clean since. The slow route is the second
  line.
- **Tiles are the selector** (`op-tiles`): requests, error rate, p95
  latency, share to projects. Each carries its value and one baseline line
  (requests/s and the split; 5xx count; p50 and p99; console share). The
  selected tile is the chart's series: requests with 5xx as the thin line;
  error rate with a 1% threshold; latency as p50 thick, p95, p99 thin, all
  ink, no legend of colours; destination as project routes with console
  thin. Deploy markers on all of them.
- **Splits are Breakdowns**, not charts: status class, destination (with
  the proxy's own answers as children: ACME, redirects), slowest routes
  (p95, no share), 5xx per upstream.
- **Facets:** routes (a Ledger: host + path, project mark, upstream,
  requests, 5xx %, p95, state with the reason) and the access log
  (LogLines, live with pause, sampled above 1k req/s and the meta says so).
- **Fresh:** the proxy always exists, so the state is empty, not
  unconfigured: no requests yet, attach a domain or open a project URL.
- The project Picker and the RangePicker sit in the actions; 1h is the
  default because the proxy is a minute-resolution surface.

### Deployment (`deploy:<tag>`)

`src/sections/ConsoleV1Deploy.tsx`. The record people open most, usually
because something is wrong, so it follows the recipe strictly:

- **Verdict** says whether traffic is on it and what changed since. Live
  with a regression: "Serving production since 20:33, but the error rate
  went from 0.12% to 0.61% after it: one new TypeError in AddressForm, 31
  events. Open the issue or roll back to dep_90e in about 5s." Failed:
  the step, the elapsed time, the compiler's clause, and "Nothing changed
  in staging." Building: "step 2 of 10, build container image. 51s so far,
  usually 2m 20s end to end", ticking. Superseded and cancelled are
  "Nothing to do".
- **Lede** word live · superseded · failed · building · cancelled, then
  commit (sha + message), branch, trigger, by, took, replicas.
- **Pipeline** is the content column: one `Stages` list, phases build ·
  release · after going live as headers, each step's line saying what it
  produced ("image 212 MB · 14 layers · 9 cached", "2 of 2 replicas
  healthy · GET / 200 in 0.8s", "798 assets · 18.8 MB"), the failed or
  running step open on its log, the rest one click away. Post-deploy
  housekeeping (cron, alerts, agents, screenshot, scan, source maps) sits
  under "after going live" and never fails the deploy. A failed step also
  gets a `Callout` above the pipeline quoting the tool verbatim with a
  retry.
- **Since it went live** is three metrics against the previous deploy;
  **Screenshot** is a framed capture with the URL (the landing mock stands
  in). **Aside** says only what the lede does not: Serving (url, resources,
  image digest, node), Source (repository at the commit, started, the
  deploy it replaced or was replaced by), Danger (roll back, delete, typed
  confirmation). Commit, branch, author, trigger, took and replicas are
  lede facts and appear nowhere else on the page. Actions are visit + redeploy, or cancel while
  building.
- **Facets**: build log (every step's lines merged, search + levels,
  download), runtime log (both replicas since this deploy, live), checks
  (a ledger of the after-going-live steps with their results).

What it drops from `web`: eleven equal cards each with a description
("Download source code from git repository") and a gear icon, the preview
above the pipeline, and equal weight for the steps that decide whether the
site is up and the ones that tidy up afterwards. Entry points: the deploys
tab rows, the project overview's recent deploys, and "Building now".

### Database (`/v1?p=databases`, `db:<name>`)

`src/sections/ConsoleV1Database.tsx`. The live page
(`web/src/pages/DatabaseDetail.tsx` and its monitoring route) is a header with
three badges, an uptime bar, then equal cards for Monitoring (seven tiles, a
chart, collapsed alert rules), Configuration, Backups and Environment
Variables, with the full metric set on a separate page. Nothing is first.

On the record recipe it is one page with facets:

- **Title row**: engine mark + name; meta is engine · version · environment ·
  node · created. Those five are said here and nowhere else: engine, version
  and node do not come back as aside rows (§7 rule 4). Actions are what you do: copy URL, back up now. Data and
  logs are facets, so they are not repeated as buttons.
- **Verdict** is the one thing to act on, and for a fresh service that is
  "no backup has ever been taken", not "Operational". A failed backup or a
  restart in the last 24h are the other verdicts; healthy says when the last
  backup was and whether point-in-time recovery is on.
- **Lede** "running" with the six facts: uptime 24h, response, the engine's
  first two metrics (memory used of limit and clients for Redis, connections
  and transactions for PostgreSQL), last backup, linked projects as a row of
  project marks only (name on hover and focus, click opens the project; the
  aside lists them with names). Facts that are the problem carry ◐.
- **Content**: Health (the 24h uptime strip, a five-metric strip where the
  selected metric is the chart's series, the chart with a range Segmented in
  its footer), Backup (one line: the last backup with its state, id, size, source and
  age; a second line with the next run, retention, PITR and how many of the
  last seven failed; restore-from-it and back-up-now. The list is the
  facet. Or an onboarding block that says what a backup is and offers
  "back up now" and "schedule"), Alert rules (a framed list, none firing).
- **Aside**: Connect (host, port, password as SecretValue, URL; one reveal for
  the section; a sentence naming the variables linked projects receive),
  Runs on (image, volume, memory limit, point-in-time recovery), Linked
  projects (or a sentence saying what linking does), Danger (restart,
  upgrade, delete; all typed; delete says whether a backup exists).
- **Facets**: backups (Ledger with restore per row, PageState empty when
  none), metrics (the full strip and a taller chart, then container
  resources), logs (LogLines, live with pause), queries (PostgreSQL:
  Histogram with percentile plus a statements Ledger sorted by share of
  total time, with a "why" column; other engines say what they expose
  instead and where to run it), data (tree + grid, browse | query Segmented).

The Configuration card and the Environment Variables card collapse into
Connect and Runs on: the reader wants "how do I reach it" and "what is it",
not "parameters". The monitoring page is gone: the health section on the
record and the metrics facet are the same strip at two sizes.

### Analytics (`/v1?p=analytics`, `event:<name>`)

`src/sections/ConsoleV1Analytics.tsx`.

First run (the **fresh** checkbox in the shell header, `?fresh=1`): the
verdict is ○ "No visits recorded yet", the meta says "no data yet", the tabs
stay, and every tab renders the same unconfigured PageState: what the page
will show (a fake verdict line, a bar strip, a breakdown line), the one
script tag to add, a note that no cookies or consent are involved, and a link
to the settings page that holds the snippet. Nothing is hidden and nothing
is blank. The live page
(`web/src/components/project/ProjectAnalytics.tsx`) is ten breakdown cards plus
separate visitors / events / campaigns / journey routes, each a top-ten list.
The redesign asks one question per facet and gives each its own form:

- **overview** answers "how is it going": four metrics, the time chart with
  deploy markers, and four five-row previews (where, how they arrived, pages,
  events) each linking to its facet. No ledger.
- **audience** answers "who": Where (country → region → city, flags),
  Language (language → locale, the locale code as the icon; an honest
  "unknown" row explains it is never guessed from country), Browser (marks),
  Device (icons). Four Breakdowns in an `op-grid`, share of all visitors.
- **campaigns** answers "did the launch work": one Ledger where a row is
  source · medium · campaign with a sparkline, visitors, signups and signed-up
  %, and term / content are variants inside the row. Untagged traffic is the
  hint sentence with a "build a tagged link" action, never a 99% bar. Rows
  under 1% signed up carry ◐.
- **pages** is the pages Ledger with a list | flow Segmented; flow is the old
  journey tab (ranked transitions, entries, exits) so the screen keeps one
  ledger.
- **events** answers "is my instrumentation alive": a Ledger with a health
  column (× stopped, ◐ far below usual, compared with the previous 7 days),
  sparkline, fires, visitors, last seen. The page verdict names the broken
  event and the deploy it stopped after. ⏎ opens `event:<name>`, a record
  page: Lede says stopped / below usual / firing, content is fires per hour
  with the deploy marker and the five most recent fires as a Timeline (or a
  sentence naming the last call site when there are none), aside is where it
  fires and its properties as Breakdowns, with a "no properties" onboarding
  sentence showing the `track` call.
- **funnels** is unchanged.
- **speed** answers "which vital, where, on what": five vital tiles (p75,
  state word, sparkline) that are also the selector, with a desktop | mobile
  Segmented because p75 differs by device; one trend chart of the selected
  vital with good/poor threshold lines and deploy markers; "by country" as a
  list (worst first, flags) with a map as the second view; and one Ledger
  with a dimension Segmented (pages · countries · regions · cities · devices
  · browsers · OS), samples plus the five vitals, sorted by the selected vital
  worst first. Cells take colour only when a vital is not good (◐ / ×); a
  row's state is its worst vital. No overall score ring: the verdict sentence
  names the vital that needs work and what drives it. Crawlers and AI agents
  are excluded and the footer says so.

Icons: flags are regional-indicator emoji in the sandbox and should be an SVG
set in the console; browser marks are monochrome line drawings (Chrome,
Firefox, Edge hand-drawn; Safari is lucide `compass`; everything else
`globe`); channels use `link` `search` `external-link` `share-2` `bot` `mail`
`megaphone`; devices `monitor` `smartphone` `tablet`. Kind is the icon, state
is the glyph, and icons never take state colour (brand §6).

### Email (`/v1?p=email`, `email:<id>`, `domain:<id>`)

`src/sections/ConsoleV1Email.tsx`, from EmailProviderResponse,
EmailDomainResponse + DnsRecordResponse, EmailResponse, EmailStatsResponse and
EmailTrackingSetupResponse. The live page (`web/src/pages/Email.tsx`) is five
equal tabs of cards with the SDK docs inside the console. On v1:

- First run (the **fresh** checkbox in the shell header, `?fresh=1`): every tab onboards instead of going
  blank. Mail: "Nothing has been sent yet" with the curl to send one and
  links to the provider and domain tabs. Domains: "No sending domain" with
  what the DNS check does. Providers: "No email provider" with what sending
  looks like. The verdict says nothing has been sent, in idle, not in warn.
- One Detail with four tabs, mail · domains · providers · settings, instead
  of the live page's five cards. Tabs split by kind of record, one Ledger per
  screen (brand §6): "mail" is what went out and what went wrong; "domains"
  and "providers" are the setup, one facet each (an earlier draft stacked
  both Ledgers on one "sending" tab and produced two filter boxes, two
  footers and two lists claiming j/k); "settings" is tracking. The verdict
  comes first: a domain whose SPF or DKIM failed (linked), the bounce rate
  above threshold, "no active provider: mail is captured, not sent".
- Mail: a metric grid with baselines (sent, delivered %, bounced with the
  threshold and "since dep_91a", opened %), one chart of sent and bounced
  per hour with deploy markers, and the sent Ledger under it. The chart is
  the time filter: drag across it and the ledger narrows to those hours, its
  footer saying so. The row says delivered / opened / bounced / failed /
  queued / captured and the first clause of the reason; a status Picker
  filters, including "problems". Opening a row is the event timeline
  (queued → sent → delivered → opened, or bounced / failed with the provider
  text), then content and headers. A hard bounce explains suppression.
- Sending, domains: a Ledger where the status cell names the record that is wrong
  and a glyph per record shows the whole set. The domain page lists every
  DNS record with copyable name and value and its own state; when SPF
  failed, a raised "what to change" block gives the exact value to paste.
  "verify now" is the one action.
- Sending, providers: type, region or host, domains served, active, default,
  with "send test" and activate / deactivate on the row. With no provider the
  section onboards (captured mode, what a send would look like, add a provider).
- Settings: a Settings page: open and click tracking as Toggles with the
  honest caveats, the webhook URL copyable, SNS topic, event destination as
  a Status. Danger zone deletes tracking data with a typed confirmation.
- SDK documentation leaves the console for the docs site; a link stays in
  the title actions.

### Nodes (`settings:nodes`, `node:<name>`, `settings:cluster`)

`src/sections/ConsoleV1Nodes.tsx`. The live Worker Nodes page is a table
where every row says "Active" in a green pill, three unlabelled bars per row
carry the pressure, and the join token, a how-to, Cluster DNS and Cluster
trust are cards above and below it.

- **The list answers "is every machine reachable and does any of them
  hurt".** Node, then **status as a word with the heartbeat age** ("online
  · 2s ago", "offline · 4m ago"), role and tunnel mode, address, size,
  pressure as three numbers in one cell (cpu · mem · disk) with colour only
  on the one that is not fine, and what is running. A node that stopped
  answering is `×` and its containers read "3 unreachable" in red: a fault
  looks like a fault. The verdict names the offline node, what it takes
  down, and the two ways out; memory pressure on another node is the `+1`.
- **The record** follows the recipe. Verdict; lede word online · offline ·
  draining with heartbeat, address, reach, agent, running, up; a Callout
  quoting the agent's last error when offline. Content: Pressure (three
  tiles, the chart of the picked one, range), Running (first four). Aside:
  Reach (private and public address, tunnel, latency), Agent (version, os,
  joined, last heartbeat, and a note when it is behind), Danger (drain,
  undrain, remove; the control plane says it cannot be). Facets: containers
  (a ledger, each row opening its project or service), agent log.
- **Cluster** is a settings page: joining (the token as a SecretValue with
  regenerate, the three commands to run on the machine), cluster dns (the
  toggle, the locked pool and prefix with why they are locked), trust (the
  CA fingerprint), and CA rotation in the danger zone with what it breaks.
  The list's hint links there and "join a node" goes there.
- Every "hetzner-1" mentioned on a database or deployment record opens
  `node:hetzner-1`; before this the link went nowhere.

### Landing system map (`/landing`, "One engine at the center")

`src/components/system-map-section.tsx`, carried over from temps-landing and
brought onto the ink system: the section uses the landing's own tiers
(`op-label` eyebrow, `op-h1`, `op-lead`, left-aligned like every other
section); node and panel frames are square 1px rules, the engine is the one
`op-raise`; the active state is ink (`border-foreground`, `text-foreground`),
not the accent, because the accent is reserved for the primary CTA; every
connector is an elbow (`elbow()`), the right-hand beziers are gone; panel
titles are `op-label`, items 12px, node subtitles mono 11px; the category
toggles are the console's tab form (square, active filled ink), not pills;
the blurred halo behind service logos is removed. The only curves left in
the section are the PostgreSQL and MongoDB logo glyphs.

### Agent conversation (`/agent`)

This is the reference surface for brand-guidelines §0 (AI-native, under
policy): an agent is an operator, so its work is a ledger of typed tool calls,
its writes are proposals with an inline approval, and its autonomy level is
said in words. Skills, MCP servers and scheduled agents get the same
treatment as a git provider: a Ledger with name, source, permissions and
last run, and an onboarding state when unconfigured.

The Vercel AI Elements vocabulary (Message, Reasoning / ChainOfThought, Plan,
Tool with its six states, Confirmation, Task, Queue, Checkpoint, Sources,
Actions, Suggestion, Context, PromptInput) drawn with the v1 rules, in
`src/sections/AgentChat.tsx`. Every block is a small component ready to move
into `src/components/op` when the console grows an agent surface.

- The transcript is a ledger of turns (who · when · model in a left column).
  Inside a turn there are no boxes. A tool call is one line: a kind icon that
  says what the thing IS (terminal for a command, pen for an edit, document
  for a read, magnifier for grep, globe for fetch, git branch for git, bot for
  a subagent, checklist for tasks, numbered list for a plan, brain for
  reasoning), the mono name and argument, and the state as a word on the right
  (preparing with a blinking caret, running, done · 38ms, failed, needs
  approval, approved, denied). Only failure and approval tint the icon and the
  word; done is quiet. Expanded input/output hangs under the line as an inset
  pane, indented, no frame. Reads, greps and fetches collapse by default;
  edits and commands are open by default because the diff and the output ARE
  the content. An edit renders its unified diff (`diff` prop): added lines in
  ink with a green `+`, removed lines muted and struck through with a red `−`,
  hunk headers muted. Colour lives only on the sign. A command shows the full
  command after `$` (wrapping, never truncated) with its complete output in
  the inset below; the command is not repeated inside the output. When the
  call needed approval the meta reads `approved · done · 2.1s`. Nested content
  (reasoning steps, a subagent's transcript) hangs off a soft left rule. File
  references are plain muted mono, not chips. Suggestions are underlined text,
  not buttons. The only framed things in a turn are the question while it is
  unanswered (the one raised element) and a destructive approval's red left
  rule.
- Approvals are inline, never a modal: approve once · always for this session
  · deny, with Y / N. Needing approval is not red. A consequential action
  (push, deploy, clear a cache) asks in ink: it says what it does to whom and
  how to undo it, and offers approve · always · deny. Red is reserved for
  the irreversible: an action that loses data nobody can get back (drop a
  database, delete a project, wipe backups). That one gets the red left rule,
  a red "run it", no "always", and a reason that ends in "cannot be undone".
  Red on a confirmation reads as "error", so if the reader can undo it, it is
  not red. The agent waits; the status line says an approval is waiting and
  links to it.
- Reasoning collapses to "thought for 6s · 3/3 steps"; open shows the steps
  with glyphs. A plan is a numbered list with file chips and approve / edit.
- A subagent is one row holding its own transcript, indented. A question is
  the one raised element while unanswered, with 2-4 options and "or type".
  Answering is two steps: pick (radio, ○ → ●, 1–4 from the keyboard), then
  "confirm ‹option›" (⏎). One click never sends an answer; a misclick mid-run
  is not reversible. The same holds for any choice the agent waits on.
- Tasks are a glyph list (done struck through) in the transcript and in the
  right rail; a checkpoint is a thin rule with "restore".
- The prompt bar is sticky at the bottom: textarea, then a row of Pickers that
  say model, thinking, permission mode, workspace in words; context as a
  quarter-circle glyph with tokens and percent, opening a breakdown with cost;
  send is the one ink fill, stop replaces it only while the agent is actually
  executing. Only then does a message queue. Waiting on an approval or a
  question is not executing: a message sent then goes now (the placeholder
  says so) and the send button stays, since there is nothing to stop. Both are derived from the transcript, never
  stored, so they cannot go stale. Typing while the
  agent runs queues the message and says so. Queued messages are full-width
  inset rows directly above the textarea, never chips and with no heading (the
  position says what they are). The text is readable in full; at the end of
  the row three icon actions with titles: pencil edits (moves it back into the
  textarea), the ink arrow sends now (interrupts the current turn), × drops
  it. Clicking the text also edits. Key hints under the bar.
- The page is a working simulator. Sending a message (or "send now" on a queued
  one) runs a fake turn of 3–10 blocks drawn at random from pools in
  `AgentChat.tsx` (reasoning, read, grep, command, failing command, edit with
  diff, fetch, subagent, tasks, checkpoint, question, plan, prose, and a
  destructive command), revealed 0.5–1.4s apart. The block under the cursor
  is shown running until the next appears; a question, a plan and a
  destructive approval pause the turn until answered, and the status line
  says so. When a turn ends the first queued message starts by itself. Stop
  ends the turn with "Stopped after step k of n"; retry re-runs the prompt.
  Use it to check how any block reads mid-run, not just at rest.
- The page fills the viewport under the docs header; the transcript is the
  one scrolling column and the right rail stays put. The transcript tails:
  a new block scrolls into view while the reader is within ~240px of the
  bottom, and a new turn always does. Scroll up to read and it stops
  following until you come back down.
- Answer actions (copy · retry · thumbs) answer in place, in words, on the
  button pressed. Copy becomes "copied" for two seconds, or "couldn't copy ·
  select the text instead" in red when the clipboard is unavailable (plain
  http on a LAN address has no `navigator.clipboard`). Retry becomes
  "retrying…" with a spinning icon and locks until the run ends; it is
  disabled while the agent is already running. A thumb turns ink and says
  "noted" (thumbs down adds "tell me what was wrong below"), the other fades;
  press again to take it back. No toasts: feedback belongs next to the thing
  it is about.

## 7c. Responsive rules

Verified at 390 and 1440 wide on every v1 screen with a scrollWidth check.

- Actions go through `ActionBar` (Detail `actions`, Ledger `action`, or
  directly). From sm up it is the right-aligned wrapping row you expect.
  Below sm it is the same row at natural widths, scrolling sideways with an
  edge fade when it does not fit, exactly like the tab strip: three actions
  are three compact buttons on one line, never full-width bars stacked into
  a pile or a two-column grid with a hole. Order is kept, primary last.
  Every action in the row must look like a button (outline or primary):
  a ghost button next to outlined ones reads as loose text.
- The command palette (and any dialog whose height depends on its results)
  is anchored at 8vh from the top with the list scrolling inside
  `max-h-[min(70vh, 640px)]`, never centred: a centred dialog re-centres as
  results change and its tail drops below the viewport on a laptop.
- Panels hanging off a header control (attention, notifications) go through
  `Drop`. Right-anchored under the control from sm up; below sm a right-
  anchored panel runs off the left edge, so it becomes fixed, edge to edge
  with 0.75rem gutters, under the control's bottom line.

- Ledger rows hide their `cells` below md and render `mobile` instead. The
  `mobile` node must carry the row's primary action too (promote, roll back);
  a phone user cannot reach a desktop-only cell.
- Rows are fixed-height on desktop (`--row-h`) and grow with content on
  phones (`@media (max-width: 767px)` in the v1 block). Never put multi-line
  content in a row and rely on the desktop height.
- Tab strips and action bars share `ScrollRow`: the row scrolls sideways
  and a fade appears on whichever edge is clipped. The active tab is
  scrolled into view on change, so ten facets work like six. Segmented controls and range
  pickers use the same `.op-scroll-x`. Key badges in tabs hide below sm.
- Tile strips (metric tiles, vital tiles) use `.op-tiles` with `--tiles: N`
  for the desktop column count. Phones pair tiles two per row; an odd last
  tile spans the row so the frame stays a rectangle and borders never
  double. Never write the border arithmetic by hand.
- Action groups wrap and take the full width below sm (`w-full sm:w-auto
  sm:ml-auto`). Never `ml-auto` alone on a group of three or more buttons.
- Custom grids using `.op-cols` collapse to `grid-cols-[1fr_auto]` below md;
  mark every secondary cell `hidden md:block` and fold what matters into the
  first cell as a second line.
- The waterfall keeps its bar on phones (full width under the span name) and
  puts the duration on the name line.
- Status line stays one line and truncates; the quiet tail is the first thing
  lost, which is correct.

## 8. Data rules

Temps sells observability. These are the core of the system.

- Numbers mono and tabular. Units follow the number. `30.8k`, `184ms`, `0.61%`.
- Time is relative under a day (`41m ago`), absolute after, with the deploy id
  beside it when one exists.
- Every time axis has deploy markers. Every delta names its baseline.
- Empty value is an en dash. Zero is `0`.
- A chart with no data says which of four reasons: no traffic, not configured,
  sampled past quota, past retention. Retention differs per plan, so the footer
  states the horizon.
- Logs use LogViewer, never a `<pre>`.

## 9. Keyboard

| Key         | Where               | Does                                      |
|-------------|---------------------|-------------------------------------------|
| ⌘K          | everywhere          | command palette                           |
| `/`         | ledger              | focus the filter                          |
| `j` `k` `⏎` | ledger              | move, open                                |
| `1` `2` `3` | detail              | switch tab                                |
| ⌘⏎          | detail              | primary action (deploy)                   |
| ⌘S          | settings            | click the save button                     |
| `d`         | everywhere          | toggle density                            |
| `esc`       | everywhere          | close drawer, menu, dialog                |

Keys are ignored while an input has focus. Every key has a visible badge.

**The cursor is the focus.** In a ledger, `j`, `k` and the arrows move DOM
focus to the row they mark, and the row's own key handler opens it on `⏎`.
The window handler only opens the cursor row on `⏎` when nothing is focused.
A cursor that merely paints a bar while focus sits on a tab or a footer link
makes `⏎` act on that other element: the reader sees a marked row, presses
enter, and lands somewhere else. Any new list with a cursor follows the same
rule: never move a highlight without moving focus with it.

## 10. Plans as design input

`ConsoleV1.tsx` has a `PlanContext` with self-hosted, Starter, Team, Business
and a switcher in the header. It exists so the same screens can be seen under
each plan's retention and ingest allowance. In the real console this comes
from the license or Cloud subscription. What changes per plan:

| Plan        | Retention          | Ingest    | PITR         | Shows when exceeded                       |
|-------------|--------------------|-----------|--------------|-------------------------------------------|
| Self-hosted | as configured      | none      | as configured| never sampled; ranges gated by config     |
| Starter     | 30d                | 10 GB/mo  | 7d           | `sampled` status, band, footer, settings  |
| Team        | 90d                | 100 GB/mo | 30d          | same                                      |
| Business    | 13 months          | 1 TB/mo then $0.30/GB under a cap | 90d | same, plus the cap in settings |

Backup storage and AI credits are not billed past the included amount today.
The UI must say so rather than imply a charge.

## 11. State of the real console (`temps/web`) and the migration

Survey numbers, so the next person does not repeat it:

| Measure                                  | Value                     |
|------------------------------------------|---------------------------|
| tsx files / page files                   | 516 / 117                 |
| Tailwind palette literals (`text-red-500`, `bg-slate-50`) | 2,265 in 189 files |
| hex literals in tsx                      | 129                       |
| files using `<Card`                      | 214                       |
| files using `<Table`                     | 52                        |
| files using `Loader2` / `<Skeleton`      | 134 / 141                 |
| empty-state implementations              | 3 (used in 44 files)      |
| `dark:` usages                           | 124 files                 |
| responsive prefixes                      | ~2,100                    |
| radius                                   | 0.5rem (system is 0.25rem)|

Migration order:

1. Land the ink tokens behind `.operator.ink` on the console shell. Nothing
   changes until the class is set.
2. Codemod palette literals to tokens. Most are mechanical
   (`text-red-500` to `text-destructive`, `bg-slate-50` to `bg-muted`).
3. Copy `src/components/op/` into `temps/web/src/components/op/`. It depends
   only on shadcn primitives already there, recharts and lucide.
4. Ship the Projects ledger on `Ledger`, then the Project detail on `Detail`.
   These are what a trial user sees first.
5. Enable the class by default. Ratchet the rest.

Done means: literal count zero, the three templates in use, the two reference
screens match `/v1` in a screenshot diff.

## 12. Enforcement

None exists yet. This is the most important open item; without it the
direction will drift the way the console already has.

- A script counting palette and hex literals in `temps/web/src`, run in CI,
  failing when the count rises. Print the number in the check output.
- Playwright screenshot diff of `/v1`, `/v1?p=api-gateway`, `/brand` in the
  design-system app on every PR that touches it.
- `temps/web/CLAUDE.md` gets a pointer to `brand-guidelines.md` and the banned
  list, so agents read the same rules as humans.
- A rule changes only by editing the doc and the reference page in one PR.

## 13. Banned

Enforced once §12 exists; documented until then.

- Tailwind palette literals and hex in tsx.
- A second hue. Colour is status, or the single landing accent.
- Spinners as page state. Skeleton for loading; a spinner only inside a
  pressed button.
- Blank empty states. Every non-happy state goes through PageState.
- Confirm dialogs that are not EchoDialog.
- A plain `<select>` for branches, images, regions, environments or anything
  with more than about seven options. Use Picker.
- Titles at weight 500. Titles are 600–800, body 400, labels 500.
- Cards as layout. Grids with ink borders; one `.op-raise` per screen.
- Hiding a feature because it is not configured or not on the plan. Show it,
  say what is missing, show an example, link to where it is fixed.

## 14. File map

```
design-system/
  docs/
    design-system-handoff.md      this file
    brand-guidelines.md           direction, type scale, colour, moves
    RULES.md                      imperative digest for coding agents (rendered at /guide#tooling)
    design-system-answers.md      the twelve questions, answered
    operator-console-brief.md     original brief (historical, do not edit)
  src/
    globals.css                   all tokens; blocks listed in §4
    components/op/                the operator library (§6, §7)
      index.ts  kbd.tsx  status.tsx  num.tsx  page-state.tsx
      echo-dialog.tsx  templates.tsx  time-chart.tsx  picker.tsx
    components/ui/                shadcn primitives + sparkline, log-viewer, empty-placeholder
    components/platform-logos.tsx, system-map-section.tsx
                                  copied verbatim from temps-landing; do not edit here
    sections/ConsoleV1.tsx        reference console (uses components/op)
    sections/ConsoleV1Env.tsx     deploys + promote, environments, variables tabs (§7b)
    sections/ConsoleV1Observe.tsx sandboxes, sandbox detail, traces, trace waterfall, metrics (§7b)
    sections/InkLandingV1.tsx     reference landing
    sections/Guide.tsx            /guide — renders docs/*.md into one consolidated page
    lib/md.ts                     the slicing helpers the guide cuts documents with
    sections/OpComponents.tsx     component reference page
    sections/Brand.tsx            brand page incl. hierarchy block
```

## 15. Open items, in priority order

1. Enforcement (§12). Nothing else holds without it. Concrete case found on
   the agent page: no component may write its own hover for a filled control.
   Fills use `.op-fill-ink` / `.op-fill-destructive`; a lint that flags
   `hover:bg-destructive`, `hover:bg-foreground` and `bg-foreground text-background`
   in `.tsx` would have caught both hover bugs (§7b, agent conversation).
2. Wire the redesigned deploys / environments / variables tabs (§7b) into the
   real console. Backend already has promote; bulk association needs an
   endpoint that takes many variable ids.
3. Screens that exist in the nav but have no template yet: Email, Uptime,
   Backups, Git providers, Security. Each is a Ledger or a Detail.
4. Landing: the copied engine section keeps its own heading and soft cards. It
   is verbatim from the live site; restyling means diverging from it.
5. Landing: links do not navigate; estimator numbers are placeholders.
6. Dark mode of the v1 console has been token-checked but not screenshot
   reviewed screen by screen.
7. `EmptyPlaceholder` and `PageState.unconfigured` overlap. Retire the former
   once the landing stops using it.
8. Accessibility pass: the ledger uses `role="listbox"` with
   `aria-activedescendant`; verify with a screen reader, and confirm the
   sampled band has a text equivalent beyond the footer.

Found by the kitchen-sink stress page (`/kitchen-sink`), not yet fixed:

- Ledger, Detail and the console shell bind keyboard handlers to `window`. Fine
  with one screen mounted; on a reference page with several ledgers `j`/`k`
  move every cursor. Scope handlers to focus-within before the real console
  mounts more than one template per route.
- Field has no error slot; an invalid input's message goes in `help`, which
  reads as advice. Add `error` and let Settings block save while any is set.
- Density has two sources of truth: `data-density` on the root and the `dense`
  boolean on Ledger. Derive one from the other.
- Ledger cursor: shown on load and following the mouse it reads as a selection
  with no consequence. Proposal: show it only after keyboard use, starting on
  the first row needing attention; mouse users get hover only.
- Responsive previews in a frame are not possible: the shell's breakpoints are
  viewport media queries, only Field uses container queries. The kitchen sink
  says so instead of pretending the 390 preset works.

## 16. How to hand back

Before saying a change is done: `bunx tsc --noEmit -p .` is clean, the dev
server has been restarted if a new class was introduced, `/v1` and
`/op-components` have been looked at in a browser at 1440 and 390 wide, and
any rule you changed has been changed in `brand-guidelines.md` and on
`/brand` in the same commit.

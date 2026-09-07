# Temps design system: handoff

This is the document to read first. It is written for whoever picks the work
up next, human or model, and it is complete enough to continue without the
conversation that produced it. Everything it describes exists in
`temps/design-system/` and can be run and looked at.

Companion documents, in reading order after this one:

- `brand-guidelines.md`: the direction, the type scale, colour, signature moves.
- `generative-ui.md`: what an AI may render. An agent answers with the console's
  own blocks, carries the call that drew each one, and proposes every write.
  The reference surface is `/agent` (§7b).
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
reference page, that renders these markdown files — this one, `brand-guidelines.md`,
`ux-audit-2026-09-06.md`, and the eight documents that own a rule each
(`forms.md`, `notifications.md`, `content.md`, `localisation.md`, `data-viz.md`,
`generative-ui.md`, `motion.md`, `icons.md`) — in the order someone building a screen needs them, with live token swatches,
the type scale in its real classes, the six status glyphs and an example
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
| `/v1?p=errors`     | Issues ledger; `issue:<id>` the record. `&fail=1` shows the error-store outage with retry; `&fresh=1` shows the no-DSN onboarding. |
| `/v1?fresh=1`      | Fresh install: every screen as the console looks minutes after `temps serve` first started — nothing configured, nothing recorded. Combines with `p`, e.g. `/v1?p=email&fresh=1`. |
| `/console?p=…`     | The console alone, no sandbox layout or intro; same `p` views. The ⤢ button in the header toggles it. |
| `/landing`         | The landing alone, no sandbox layout. The ⤢ button fixed bottom-right toggles it (sandbox control, not part of the page). |
| `/v1?p=settings`   | Settings hub; `settings:<slug>` pages (domain, updates, builds, timeouts, users, teams, signin, keys, headers, traffic, routes, store, retention, alerts, nodes, plugins). |
| `/status?project=` | The public status page for a project, chrome-free; `/status-page` inside the sandbox. `/v1?p=monitor:mon_2` is a monitor record. |
| `/v1?p=sandboxes`, `?p=sandbox:sbx_7f21`, `?p=traces`, `?p=trace:3f9c1e7a8b2d4f60`, `?p=metrics` | Observe and sandbox surfaces (§7b). |
| `/v1?p=logs`, `?p=log:<id>` | The Logs tool screen: query bar, volume chart, facets, three renderings of one list, and one line as a record (§7b). |
| `/agent` | The agent conversation, in two scenarios: a coding agent in a worktree, and the console assistant answering with generated blocks (§7b, `docs/generative-ui.md`). |
| `/v1-landing`      | Landing page in the same system, with pricing.                         |
| `/op-components`   | Every operator component, every state, with props.                     |
| `/brand#hierarchy` | The type scale rendered live.                                          |
| Docs | `docs/forms.md` · `notifications.md` · `content.md` · `localisation.md` · `data-viz.md` · `generative-ui.md` · `motion.md` · `icons.md`. One document per rule set, each a `/guide` section (`#forms`, `#notifications`, `#content`, `#locale`, `#dataviz`, `#generative-ui`, `#motion`, `#icons`) with its live blocks from `src/sections/blocks/` mounted under the prose. `/op-components` mounts the same blocks. |

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
2. **Every border is ink.** 1px `--border` equals `--foreground` on paper.
   On night `--border` (and the raise, which falls in `--border`) is 62% ink:
   a light stroke on a dark ground carries more weight than an ink stroke on
   paper, so an equal-contrast frame reads as a glowing box. The only other
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

**Hover and selection lift the whole row.** A hovered, selected or focused
`.op-row` (and an option, and a tab) overrides `--muted-foreground` to 80% ink;
an ink-filled selection — the palette's current item, a filled tab, anything on
`.bg-foreground` — takes it the other way, to 78% paper. Every utility reads
that one variable, so the row's muted text, its state glyphs and its lucide
icons all step up together. Without it the right-hand side of a row stays at
resting muted while the fill moves under it, and the half of the row the reader
is pointing at is the dimmest thing on the screen.

`web/packages/op/tokens.json` is the same layer as data (W3C DTCG, exported as
`@temps-sdk/op/tokens.json`): `base` is the raw material — the paper/ink pair,
the five state hues, the faces, radius, border, the 4/8/12/16/20/24/32 scale,
the six type tiers and motion — and `semantic` is exactly the custom properties
`.operator.ink` declares, light and dark, aliased to base with `{base.x.y}`.
`node web/packages/op/scripts/tokens.mjs check` parses both files and fails with
a diff on any differing value, any name present on one side only, and any
ordering difference; it runs inside the design system's `bun run lint`
(`bun run tokens:check` alone). `tokens.mjs build` prints the block it would
generate — op.css is still hand-written and still the source of truth, so this
release enforces the mirror rather than generating it. The token table at
`/guide#tokens` and `/op-components#tokens-table` is built from the JSON, not
from a copy of it.

Motion is three tokens and one curve: `--op-duration-fast` (80ms, hover),
`--op-duration` (100ms, the default for a control's own state change),
`--op-duration-slow` (200ms, for something arriving on top of the page) and
`--op-ease` (`cubic-bezier(0.2, 0, 0, 1)`). `.op-motion` / `.op-motion-fast` /
`.op-motion-slow` opt one element into a tier. A literal duration in a tsx file
is a bug: `[transition-duration:var(--op-duration-slow)]` is how the dialog and
alert-dialog surfaces say 200ms. One media rule in `op.css` zeroes all three
under `prefers-reduced-motion: reduce` — motion is never gated in JavaScript.
`docs/motion.md` has what may move, what never moves, and the two exceptions
(`.op-raise`'s hard 3px offset, and `animate-pulse` / `animate-spin`).

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

### Scrollbars

The platform scrollbar is a rounded grey pill; the system is square ink. The
skin therefore draws every scrollbar: 8px, square, ink at 30% on a transparent
track, 55% under the pointer, and the document scrollbar follows when the skin
owns the page. Sideways strips (`.op-scroll-x`: tabs, action bars) scroll with
no bar at all; the clipped last item is the affordance. A scroll region is
still a focusable region with a visible focus ring (axe requires it), the bar
is not the only sign that it scrolls.

## 5. Status vocabulary

`src/components/op/status.tsx`. Six states, one glyph each, one colour each.

| State     | Glyph | Colour  | Meaning                                              |
|-----------|-------|---------|------------------------------------------------------|
| `ok`      | ●     | success | healthy, passing, deployed                           |
| `warn`    | ◐     | warning | degraded, above threshold, expiring                  |
| `error`   | ×     | destructive | failing, unreachable                             |
| `running` | ◉     | ink     | work in flight: building, restoring, scanning        |
| `idle`    | ○     | muted   | not deployed, not configured, nothing yet            |
| `sampled` | ◌     | muted   | telemetry head-sampled past the plan allowance       |

`sampled` exists because the pricing page promises that past the allowance
"telemetry is head-sampled and the console says so; it is never silently
dropped". That promise is a UI contract. It shows in the status line, as a band
on the chart, in the chart footer and in project settings.

`running` is the sixth state and the only one that is **ink and never a hue**.
The other five are verdicts — this is well, this is not, this is nothing yet —
and a verdict is what a colour is for. Work in flight is not a verdict: a build
that is running is neither good nor bad, and tinting it amber says a thing has
gone wrong that has not. Its word comes from the operation, never from the
state: `building`, `restoring`, `scanning`, `importing`, not "in progress".
The glyph carries `.op-pulse`, a slow opacity-only pulse (~1.6s) zeroed under
`prefers-reduced-motion`, because motion here means work is happening now, and
it stops when the work stops (`docs/motion.md`).

**Pending, queued and waiting-for-you are `warn`, not `running`, and warn does
not pulse.** A deploy waiting on an approval, a schedule that has not fired, a
proposal nobody has answered: nothing is happening, somebody has to act, and a
pulse would say the opposite. `running` is reserved for a machine that is
actually doing the work right now.

`STATE_RANK` orders lists by attention and places `running` between `warn` and
`sampled`: something in flight outranks a healthy row, because it is about to
change, and is outranked by anything that has already gone wrong.
`worst(states)` picks the status line glyph.

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

### Button

The shadcn button in the ink skin, plus one addition the system needs:
`busy` and `busyLabel`. While busy it spins its own icon (`.op-busy`,
`0.9s linear` — the one spin in the system), swaps its label for the verb in
progress ("saving…", "reloading…", "deploying…"), locks its min-width to the
width it had idle so the row does not move, sets `aria-busy` and swallows
clicks. It is **not** `disabled`: disabling greys the control out and drops
focus, so a keyboard reader who just pressed ⌘S is thrown to the top of the
document at exactly the moment they are waiting to hear what happened. A
minimum busy time of 400ms stops a fast answer reading as a flicker, and under
`prefers-reduced-motion` the icon holds still and the label carries it alone.

A reload spins because the thing it stands for goes round; a `running` glyph
pulses (§5) because a state is not an action. `Settings` takes `saving` and
passes it to the sticky save bar. See `docs/motion.md` exception 4.

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

Series are told apart by pattern, never by hue. `Series` takes
`stroke` (`'solid' | 'dashed' | 'dotted'`) and `weight` (`'thin' | 'regular'`),
defaulted by position (solid regular, then dashed, dotted, solid, each thin).
**`stroke` used to be a CSS colour and is now the dash pattern**; `width` still
takes an exact pixel width and still wins. `--chart-1` / `--chart-2` are gone
from the component: every line is ink, and a line takes a tone only when
`series.state` says the series *is* a state — an error rate read against its
threshold band, not a line told apart from its neighbour.

The legend is generated from `series`: the swatch is a sample of the real line
(same dash, same weight, same ink), the name is muted, and the value at the
cursor rides the label, so the legend is a readout too. A legend typed into a
`ChartFooter` ("thick p50, thin p99", "the thin line is users") is now always
wrong — it cannot be matched to a line and it drifts. `legend` defaults to on
with more than one series; more than four series logs a dev warning, because
four dash patterns is what the eye separates.

`table` (default on) puts a "table" toggle beside the legend that swaps the plot
for the same buckets as an `.op-rows` table — bucket · value per series, deploy
markers in the bucket cell, same height, no animation. Every chart is readable
as numbers. The plot is `role="img"` with an `aria-label` sentence built from
`title`, `range` and `verdict`, falling back to the series names and the axis
bounds, so a chart is never an unlabelled graphic. `docs/data-viz.md` is the
whole rule set; `/guide#dataviz` draws it.

`thresholds={[{ y, label, state }]}` draws dashed horizontal reference lines
labelled at the right edge in the state tone (a vital's good / poor line).

`band={{ lower, upper, label }}` draws an expected range behind the line as two
keys of the same points, hatched in ink — never a filled area and never a
second hue — with its own generated legend entry. `anomalies={[{ x, note,
state }]}` puts a × on the first series at each called-out point; the caller
lists the same points under the plot, because a glyph on a plot is not
reachable by a keyboard on its own. `outside(point, band, key)` and
`vsExpected(point, band, key)` are the helpers that derive both from the data
("+141% above", "inside"), so the plot, the list and the table cannot drift.
`compare={{ label, data }}` merges the period before this one onto the same
points under one reserved key and draws it as a dotted thin ghost, with the
delta and its baseline in the legend ("+9% vs prior 7d"); compare equal-length
windows or say nothing. `Series` gained `top` (an out-of-band segment belongs
last in the legend and on top of the line it marks) and `inTable` (off for a
series that is a derived copy of another; a column of en dashes is not a fact).
`BandChart` wraps all of this for the metrics explorer.

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

### Field, FormErrors

`Field` carries the whole anatomy: `label` (always visible, weight 500), `hint`
(`help` is kept as the older name for the same line), `error`, and `optional` —
the console's forms are mostly required, so the exception is what gets marked.
The error renders under the hint as glyph + sentence in the destructive tone,
the one place a field carries colour, and the hint stays put while it shows,
because advice and fault are different things. Pass `id`, or use the render-prop
form (`{(c) => <Input {...c} />}` with
`FieldControl = { id, 'aria-describedby', 'aria-invalid' }`), and the control is
wired: the hint and the error are described-by, never folded into the control's
accessible name, and the label switches from wrapping to `htmlFor`. A field with
neither hint nor error renders exactly as before, at the same height.

`FormErrors` is the summary a form shows when more than one field fails on
submit: one error `Callout` at the top, each entry a button that moves focus to
the field it names (`errors={[{ id, label, message }]}`, `min` failures before
it appears, default 2). The inline message under each field stays where it is —
the summary is a way in, not a second copy of the truth. Validation timing,
disabled controls, long submits, destructive submits and secrets are all in
`docs/forms.md`; `/guide#forms` and `/op-components#form-field` show them live.

### DateTimeField, DateField, TimeField, DateTimeRangeField, DurationField, ScheduleField

`datetime.tsx`, the moment-in-time half of `Field`. All of them compose `Field`
and all of them are typed entry first: a real `date` / `time` /
`datetime-local` input under the ink skin, so a stamp copied out of a log can be
pasted, `↑`/`↓` step the focused segment, and the browser's own picker is the
accelerator rather than the only door. The value is an ISO local stamp
(`2026-09-06T20:33`), written back ISO-ordered by `fmtStamp`.

`DateTimeField` takes `zone` — rendered as a mono fact beside the control, or as
a `Picker` in the same `Field` when `onZoneChange` is passed, because a control
that guesses the clock restores to the wrong second. `precision: 'second'` adds
`step=1` for the one second-precise operation. `min`/`max` state the window in
the hint once and fault on blur with the state word and the fact. `presets`
(`now`, `−1h`, `last backup`) are a `Strip` that fills the absolute field, which
stays the truth about what they wrote. `never` makes "no expiry" an option word,
so an empty date never means forever. `DateField` and `TimeField` are the same
control at day and time-of-day precision.

`DateTimeRangeField` is two of them on one row (stacked below sm) with `quick`
windows, `to > from` validated on blur of "to", and windows past `retentionDays`
struck through with the plan word and routed to `onGated` — the same gating
`RangePicker` does, through the same `Strip`.

`DurationField` is a number plus a unit `Picker` (`s` `min` `h` `d`) over a
millisecond value, previewed with `fmtDuration`: `30d`, `30 days`, `720h` and
`30` are four spellings of one value and three of them are a parser bug.

`ScheduleField` is `HH:MM` plus its zone plus optional weekday toggles, and it
prints the next three runs underneath (`nextRuns`) so a schedule is verifiable
before it is saved. `cron` is an advanced entry behind a text button, never the
only way in. The rules are `docs/forms.md` §"Dates, times and ranges";
`/guide#forms` and `/op-components#form-datetime` show them live.

### fmt (`fmt.ts`)

`fmtNum`, `fmtPct`, `fmtBytes`, `fmtDuration`, `fmtRelative`, `fmtAbsolute`,
`fmtStamp`, `fmtCount` and `EMPTY`: pure functions, no React, no state, one locale argument,
holding the number, date and duration rules of `docs/content.md` in one place.
Locale grouping through `Intl` (never a hand-rolled separator), decimal bytes by
default and binary on request (`MiB`, where the kernel counts), percentages at
one decimal, durations in at most two units, time relative under 24 hours and
absolute after, `fmtStamp` for the ISO-ordered wall clock a date input reads and
writes (converting nothing, because the value is already in the zone named
beside it), plurals through `Intl.PluralRules` (never `+ 's'`), nothing as
an en dash and zero as `0` — different facts. `Num`, `Pager`, `Breakdown`,
`Funnel`, `Flow`, `Histogram` and `TimeChart` / `RangePicker` format through
them, and so do the sandbox screens: `toFixed` and `toLocaleString` in a screen
are banned. `/op-components#content-fmt` prints the output table.

### Sparkline, LogViewer, EmptyPlaceholder

In `src/components/ui/`. Sparkline for ledger cells. LogViewer has a gutter,
level colour, `/` search, n/N, follow toggle. EmptyPlaceholder is the older
onboarding component; PageState `unconfigured` supersedes it for new work.

### The ink vocabulary (`viz-ink.tsx`)

What every figure in the second wave is built from, so fourteen primitives look
like one system. `Figure({ label, table, footer, legend, height, children })`
is the frame: the `role="img"` plot with its `aria-label`, the generated
legend, the footer, and the table toggle that swaps the drawing for the same
numbers. `DataTable({ caption, head, rows, numeric })` renders that table view
as a real `<table>` with the numeric columns right-aligned and tabular, so
"ship a table view with every chart" is one call, not a per-figure decision.
`InkPatterns()` mounts the SVG defs once per page; `INK_LAYER_ORDER`
(`solid` · `hatch` · `dot` · `cross`), `INK_FILL`, `INK_FILL_OPACITY` and
`INK_FILL_WORD` are the four layer fills a composition may use and the words
the table calls them, which is why a fifth layer has nowhere to go.
`INK_STEPS` is the five-step density ramp, `inkStep(value, max)` picks the
step and `inkCell(value, max)` returns the cell style; zero is the empty step,
never a light something. `INK_TONE` is the only place a figure reaches a
state colour. `StateWord({ state, children })` prints glyph + word together so
neither travels alone. `useReadout(count)` is the shared pointer / touch /
keyboard readout state (`←` `→` move it) and `ReadoutLive({ text })` is its
`aria-live` line, which is how `CalendarHeatmap`'s cells stopped being a
`title` and nothing else.

### BandChart, StackedInk, LatencyHeatmap, StateTimeline, WindowTimeline, SessionTimeline

The time-shaped figures (`viz-time.tsx`). All take `title`, `range` (where
they have one), `verdict` and an optional `footer`, and all ship a table view.

`BandChart({ data, actual, actualName, band, worse, errorAt, bandNote,
markers, unit, title, range, verdict, height, xInterval, footer, onOpen })`
answers "is 980ms unusual for 10:00?": the expected range hatched behind the
line, the out-of-band stretch toned because it *is* a state, a × at each peak,
and the excursions derived from the data as `Excursion` rows (`x`, `value`,
`bound`, `pct`, `side`, `state`, `points`, `deploy`) listed under the plot and
summarised in the footer with the deploy beside. `worse` says which side is
bad (`up` · `down` · `both`); `errorAt` is the percentage past the bound that
turns a warn excursion into an error.

`StackedInk({ data, layers, unit, title, range, verdict, height, xInterval,
partial, footer })` is composition over time as stacked **bars from zero** —
never a stacked area. An `InkLayer` is `{ key, name, fill, state }`; four
layers at most, told apart by pattern, and `state` only on the layer that is
itself a state. `partial` hatches the bucket still filling.

`LatencyHeatmap({ columns, rows, counts, unit, title, range, verdict,
overlays, cell, footer })` is time × latency bucket with ink density by count,
`rows` as `{ le, label }` upper bounds and `overlays` for p50/p95 lines drawn
over it; it answers whether the p95 is one slow route or all of them.

`StateTimeline({ segments, title, range, verdict, height, footer, onOpen })`
draws `StateSegment`s (`state`, `word`, `from`, `seconds`, `note`) as wide as
they were long, with the durations printed. **`StatusStrip` versus
`StateTimeline`**: `StatusStrip` belongs in a ledger, where every row gets
equal buckets and rows are compared by shape; `StateTimeline` belongs on a
record, where the transitions are real and the question is how long it was
down. Never both for the same window on one screen — two pictures of one hour
that disagree about the width of a minute.

`WindowTimeline({ from, to, covered, marks, cursor, title, verdict, zone,
height, footer })` puts what a restore can reach on the same axis as the
restore cursor, backups as `WindowMark`s, the covered span as a band, and the
zone printed; it goes directly above the point-in-time field.

`SessionTimeline({ duration_ms, events, position_ms, onSeek, title, verdict,
footer })` is one session: the axis, and beside it the `SessionEvent` list
(`at_ms`, `kind`, `label`, `state`, `note`) that carries the keyboard. The
axis is ticks (page view a full rule, other events short, errors red), never
glyphs; the scrubber is a native range input spanning exactly the track.

### PercentileLadder, CohortGrid, DeltaTable

The grid-shaped figures (`viz-grid.tsx`), which are tables first.

`PercentileLadder({ rungs, unit, label, meta })` puts a distribution in a tile:
`Rung`s of `{ name, value, delta, baseline, state }`, each delta on its own row
beside the baseline it is a delta from. `CohortGrid({ cohorts, periodLabel,
label, verdict, meta })` is retention as a real `<table>` — `Cohort`s of
`{ label, size, values }`, the percentage in the cell, the cohort size in its
own column, and an en dash for a period not yet reached. `DeltaTable({ rows,
before, after, label, meta })` is the release comparison: `DeltaRow`s of
`{ metric, before, after, unit, better, threshold, note }`, toned only where a
`threshold` makes the value a state, never because a number went up.

### PathTree, Topology

The graph-shaped figures (`viz-graph.tsx`). `PathTree({ root, label, verdict,
dropAlert, onOpen, meta })` draws journeys as an indented, collapsible tree of
`PathNode`s (`label`, `count`, `exits`, `children`, `note`) with drop-off per
branch and `dropAlert` (50 by default) the share that takes a tone. Never a
Sankey. `Topology({ nodes, links, label, verdict, onOpen, height, meta })`
lays `TopoNode`s out in deterministic layers (`layer` is given, never solved
for, so the picture does not move between reloads) and puts the same nodes in
a list beneath it; the list carries the keyboard, and the graph is one
`role="img"` with no focusable children. `TopoLink` takes `kind`
(`direct` · `relay` · `call`), which is the pattern it is drawn with.

### UsageBar, Gauge

Measures against a limit (`viz-usage.tsx`). `UsageBar({ label, used,
allowance, unit, plan, format, sampledFrom, sampledLabel, resets, warnAt,
action })` states the usage as a sentence before the bar, hatches the overage
rather than pinning the bar at 100%, marks where sampling began, and always
names the plan and its allowance. `Gauge({ label, value, max, unit, of, peak,
peakLabel, thresholds, idle })` is a machine's pressure drawn horizontally
from zero, with threshold ticks that carry their own words and the peak with
its window; `idle` is what an unsampled node says instead of disappearing —
it keeps its tile and states why there is no number.

### ToolRow, Proposal, Provenance, StreamBlock, AgentQuestion, AgentSources, RunAside

The agent surface (`web/packages/op/src/agent.tsx`); the whole rule set is
`docs/generative-ui.md`, drawn on `/agent` and `/op-components#genui-ledger`.

`ToolRow({ name, arg, kind, state, ms, input, output, diff, error, meta,
defaultOpen, approval, approved })` is one typed call as one row: kind icon ·
name and argument in mono · state word · duration, opening to its input, its
output, its diff or its error. `state` is a `ToolState` and the word comes from
`TOOL_STATE`, never from a call site: `preparing` · `running` · `done` ·
`failed` · `needs approval` · `approved` · `denied`. `kind` is a `ToolKind`
with one icon each (`toolKind`/`toolIcon` derive it from the name). Edits and
commands open by default because the diff and the output are the content;
reads, searches and queries collapse. `approval` is a `ToolApproval`
(`reason`, `destructive`, `onRespond`) answered inline with `Y` / `N`; only
irreversible loss takes the red left rule.

`Provenance({ tool, when, range, note, query, queryLabel, children })` wraps
**every** generated block and prints `from <tool> · <when> · <range>` under it,
with `show query` revealing what was actually sent. `tool` and `when` are
required, so the component cannot be used to launder a picture the model
invented.

`Proposal({ action, target, consequence, reversal, irreversible, autonomy,
kind, confirmWord, confirmLabel, declineLabel, steps, decided, onConfirm,
onDecline })` is how a write is shown and not performed: the four facts as a
`KeyValue` (action · target · consequence · reversible), the autonomy level in
words, and nothing running until a human confirms. Reversible confirms in ink;
`irreversible` routes through `EchoDialog` with the name typed out and the
`steps` ticked.

`StreamBlock({ kind, label })` holds the shape of the block that is coming
(`text` · `chart` · `ledger` · `detail` · `keyvalue` · `tool`) at the height it
will land at, static — a skeleton that shimmers and a chart that draws itself
are both banned. `AgentQuestion({ q, options, answer, onAnswer, hint })` asks
with two to four typed options, each with the consequence of picking it,
answered in two steps: pick (`1`–`4`), then confirm (`⏎`). `AgentSources({
items })` ends an answer with what was read, as links into the console's own
records. `RunAside({ model, workspace, mode, modeState, context, checkpoints,
rows })` is the run as reference facts — model · workspace · permission mode ·
context with its percent · checkpoints, plus whatever else this run needs
stated (tools, proposals, cost).

### CopyAction

The copy control. `value` is the string or a function that builds it at press
time (a query from the current tokens). It writes the clipboard itself and
answers **on the button**: `✓ copied` for two seconds, then the verb again; or
`× couldn't copy` in red with the reason as the title when the clipboard is
unavailable (plain http on a LAN address has no `navigator.clipboard`; the
fallback selection copy can refuse too). The idle label stays in the layout
invisibly under the answer so the width never moves; the answer is a polite
live region so a screen reader hears it without the focus moving. Pass the
row's sizing classes (`h-7 text-xs`); the border, hover and states are its
own. `useCopy(value)` is the hook underneath, for a control that is not a
button (the name badge on a record title). A copy never toasts.

---

### Inspector

The panel that reads one row **beside** the list. ~520px at `xl`, where it
**pushes** the main column rather than covering it; below `xl` it is an
overlay sheet with a scrim; below md it is full screen. An ink border on its
left edge, no card and no shadow — it is a region of the page, not a thing
floating over it. The header is state glyph + word · the title as a mono id ·
the meta (the time, with the deploy id beside it) · `open` (go to the full
record page), `copy link` (a `CopyAction`: it copies the row's address and
says `copied` on itself), and `×`. The body is stacked `Section`s and **no
tabs**, with a small in-panel toc row above them — `fields · trace · request ·
context` — whose entries are anchors reachable with `1`–`4`.
`role="complementary"` with an `aria-label`, and the title is
`aria-live="polite"` so a panel following the cursor is announced rather than
silently swapping under the reader.

The keyboard contract is the whole point of it: `⏎` on a ledger row opens the
panel; while it is open `j`/`k` keep moving the **ledger's** cursor and the
panel follows; `esc` closes it and returns focus to the row it came from; `/`
still goes to the page's query bar and never into the panel; and focus enters
the panel only with Tab, so opening it does not take the list away from the
reader.

## 7. The three page templates

`src/components/op/templates.tsx`. Every console screen is one of these. A
screen that does not fit is a reason to extend a template, not to start from a
blank div.

Changing a tab or a facet never moves the document; only the content below the
strip changes. The strip reveals its own active tab sideways (`revealInRow`),
because a `scrollIntoView` on a tab also scrolls the page vertically, and a
reader who pressed `2` did not ask to be moved.

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
record it is (database engine, control plane / worker node, span kind), drawn
in a fixed 16px slot at the head of the first cell and before the name on a
phone, in muted ink. It rides the first cell rather than taking a column of
its own, so no `grid` string changes and no single-kind ledger carries an
empty slot. It is required when the list mixes kinds and left off when the
ledger's title already names the kind (the deploys of one project) or when the
row already carries an identity mark: the projects ledger shows the project
mark and says `worker · production` in the meta rather than stacking two marks
before the name. The state glyph stays where it is; an icon never carries a
state colour. Used for databases,
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

**A tool screen inspects in a panel; a record is a page.** On a screen whose
whole job is narrowing one list (logs, traces, the proxy access log, the audit
log), reading a row must not cost the reader their place in it: `⏎` opens an
`Inspector` beside the list, the panel follows the ledger's cursor as `j` and
`k` move it, and `esc` puts focus back on the row. The panel never takes `/`,
`j` or `k` — those belong to the query bar and the list, and a panel that
stole them would make the screen's own keyboard stop working the moment it
opened. The division of labour: the **panel** is for staying in the list while
reading one row, the **record page** is the deep link you send to somebody, and
the panel's `open` action goes to it. Both render the same content from one
shared component, so the thing you read in the list and the thing you paste
into a ticket cannot drift apart.

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

**The templates own the URL.** A template's own controls are view state, so
they are read from and written to the query, never held in a component: the
`Ledger`'s sort, page and filter (`?sort=` `?page=` `?f=`) and the `Detail`'s
facet (`?tab=`), with the range (`?range=`) and the row open in an `Inspector`
(`?row=`) beside them. Defaults are omitted, a view change replaces and a
navigation pushes, and a screen writes several keys in one patch rather than
calling `setParams` twice from the same snapshot. The shared hook is
`design-system/src/sections/console-url.ts`; the rule and its test are
`docs/requirements.md`.

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
for every non-ok state, so a `sampled` tile is visibly sampled. A section that
asks for a moment or a length uses the `datetime.tsx` fields rather than an
`Input` with the unit in the hint: API key expiry is a `DateField` with "never"
as an option word, retention is a `DurationField`, and a backup window is a
`ScheduleField` that prints its next three runs.

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

### Logs (`/v1?p=logs`, `log:<id>`)

`src/sections/ConsoleV1Logs.tsx`. Every line every application on this
instance wrote, in one list, with a query bar in front of it. This is the
first screen in the console that is a **tool** rather than a resource: the
reader does not arrive for a facet of something (a project's deploys, an
issue's events), they arrive with a question — "what did billing-worker say
after dep_31c" — and narrow one list until it answers. So there are no tabs,
there is no record the page is about, and the whole screen is a query bar,
one chart, one Ledger and a column of facets.

- **The scope is a sentence.** The meta reads
  `all projects · production · runtime · 24h · 422 lines`, assembled from the
  scope Pickers (projects, environment, source) exactly as §7 says a scope
  should read. Two projects makes it `in api-gateway, billing-worker`, and the
  projects Picker then carries that phrase as its selected option with
  "from the facets" as its meta — a control that cannot say the truth is
  worse than no control.
- **The query bar owns `/`.** It is the only thing on the page bound to it,
  which is why the `Ledger` is given no `filter`/`onFilter`: a second search
  box fighting for the same key is how a reader learns not to trust a
  shortcut. Free words search the message; `key:value` becomes a removable
  chip. Typing opens a `Drop` of typed suggestions — keys first with what each
  one means, then values with their line counts — and `⏎` adds the highlighted
  one, `backspace` on an empty input removes the last chip, `esc` closes.
- **Tokens are the truth, and the truth is in the URL.** Every facet row,
  every scope Picker, every saved query, every pattern row and every `Phrase`
  in the verdict writes a token. The query and the chosen columns live in
  `?q=` and `?cols=` beside `?p=`, so a log search is a link that still
  finds the same lines tomorrow. Tokens of one key are an "or" (two project
  chips mean both projects); different keys are an "and".
- **Facets are Breakdowns that add tokens.** The aside is level, project,
  environment, source, service, node, deployment and "fields seen" (the
  structured attributes: status, method, route, duration, each drilling into
  its top values). Every row carries its count, its share bar, and — where it
  has any — a second thin ink bar for the **error share** inside that group,
  so "which container is the one that is angry" is answerable without
  clicking. A small "filter facets" input sits at the top; a facet with no
  matching row does not render an empty frame. The facets count the window,
  not the query, so the reader can always widen. Below xl the whole aside
  becomes a `Drop` behind a "facets" button, so it is never both places at
  once and a phone never scrolls sideways to reach it.
- **The chart is the time filter.** Volume by level in 30 minute buckets,
  four series told apart by pattern and never by hue: error solid regular and
  the only one with a tone (it is a state), warn thin dashed, info thin solid,
  debug thin dotted. Buckets are aligned to the half hour, not to "now", so
  the deploy markers land on the rise they caused. Drag selects a window and
  the list follows it, saying so in the hint. The chart answers the query
  (a facet click narrows the plot and the list together) but deliberately
  ignores its own selection, because a plot that redrew itself from its
  selection leaves no way back.
- **Views are renderings of one list, not tabs.** A `Segmented` of
  `list · patterns · by service`, all three driving the same `Ledger` — same
  keyboard, same footer, different columns. **patterns** groups the query by
  message template (`health check GET ‹path› timed out after ‹n›s`) with
  count, share, first seen, last seen, a 24h `Sparkline` and the worst level
  in the group as its glyph; `⏎` adds a `pattern:` token and drops back to
  the list. **by service** is the same ranked-list idea for containers, with
  the error count beside the line count. A thousand lines that are one thing
  should read as one thing that happened a thousand times.
- **The row.** Time relative under a day with the absolute stamp as its
  `title`; level as glyph + word (× error, ◐ warn, ○ info, and `debug` as a
  muted word with no glyph to spend); the project's mark with its container;
  the message in mono, ANSI already stripped, truncated with the full text on
  hover. Trailing cells are the reader's choice — a `columns` Drop offers
  deployment, trace, node, request, duration and status, and the choice rides
  in the link like the query. A row with a trace shows the link glyph, and
  `t` on a row opens its trace directly. Below md the cells go and the
  phone row carries level · service · time on one line and the message on the
  next.
- **`⏎` opens the line in the `Inspector` beside the list**, which follows
  the cursor as `j` and `k` move it, so reading twenty lines does not cost
  twenty navigations; `log:<id>` stays the deep link, and it is what the
  panel's `open` action goes to. **`e` expands the line under the cursor**
  into the raw line (or the JSON re-indented) and its structured fields
  without opening anything. `space` toggles live. All three have a visible
  badge with a handler behind it.
- **Live tail.** `Live` beside the range picker, off by default. The list is
  newest first, so a pinned cursor means the reader is standing where the new
  lines land: while they are at the top, lines append there; scroll away to
  read something older and the tail holds and counts —
  "paused · 12 new lines · resume" in the ledger's hint — rather than moving
  the ground under them.
- **Actions do.** `copy query` (the same string the URL carries),
  `save view` (a name field in a `Drop`; the saved query joins the saved-query
  Picker beside the scope) and `export` (`csv`/`json`, the current query, with
  the count spelled out on the button and the 100,000-line cap stated
  underneath). Each is one click and a toast, because each is reversible;
  nothing here needs an `EchoDialog`.
- **Correlation is inline and both ways**: a line shows its trace (the
  waterfall, with the emitting span marked `▸ this line`) and its request (the
  proxy access entry), and a trace record shows the lines it produced; a line
  with no trace id says so and links the setting that turns tracing on. The
  query bar accepts `trace:` and `request:`, so a trace id pasted into it
  lands on the lines of that trace. A deployment's runtime log facet has
  "open in Logs", which goes to `logs:deploy:<tag>` with the token already
  set.
- **States.** `?fresh=1` is unconfigured: no container is running and no
  domain is on the proxy, with three example lines and the link to the log
  store. A query that matches nothing is `empty` and says how many lines are
  in the window, what it was matched against, and that lines past retention
  were deleted rather than hidden. Ranges past the plan's retention are struck
  through in the `RangePicker` and say which plan keeps them; the footer
  states the horizon and the zone (`times are UTC`) beside every window.

**Two departures from the brief, taken deliberately.** Saved views are a
`Picker` and not part of the `Segmented`, because the `Segmented` holds the
three renderings of the list and saved queries grow without limit — every
"save view" adds one — so they read as a scope, and a scope is a `Picker` read
as a sentence; a `Segmented` would break its own 2–4 rule on the fifth save.
And "by service" is the `Ledger`'s third rendering rather than a `Breakdown`,
because it looks like a Breakdown but it is the list's own rows, so `j`/`k`/`⏎`
still work and the footer still counts; a `Breakdown` dropped into the list's
place would have taken the keyboard away from the screen's main list.

**Log record (`log:<id>`).** One line is a record, because an operator quotes
one line to somebody. Meta is `log_… · project · environment`; the verdict
says what to do about it ("billing-worker has said this since dep_31c
shipped — read the lines around the deploy, then roll it back or fix the
check"), and an ordinary info line gets "Nothing to do: an ordinary runtime
line, kept so the twenty around it can be read". The Lede carries level,
service, deployment, node, trace and when it was written. Main column: the
line itself (raw as the container wrote it, or the JSON re-indented), the
structured fields as `KeyValue`, and **±20 lines from the same container** as
`LogLines` with `▸ this line` in the source column — which is exactly what
`log_chunks.line_offsets` exists for in the backend. Aside: where to go
(the trace, the deployment, the issue error tracking grouped it under, every
line this container wrote, the rest of this request) and "33 lines in the
hour before this one say exactly this. One alert covers all of them."
Actions: copy line, open in trace, create alert from this query.

**Real shapes behind it** (read from `temps/`, never edited): `log_events`
(time, project_id, service, env, level, message, `fields` JSONB, chunk_id,
line_offset, deploy_id), `log_chunks` (container_id, node_name,
external_service_id, started_at, line_count, line_offsets) and
`deployment_container_logs` (container_name, service_name, node_id). That is
why a line carries a service, a node, a deployment and a chunk offset, and
why "the twenty lines around this one" is a real query rather than a nicety.
Real console pages this replaces: `components/runtime-logs/log-viewer.tsx`,
`history-log-viewer.tsx`, `components/containers/ContainerLogsViewer.tsx` —
none of which is cross-project, and all of which are one container at a time.

**Fixtures.** ~420 lines, seeded and clock-fixed at 2026-09-06 21:29 UTC, so
a visual baseline is a fact about the design and not about the hour it ran
in. Four applications — api-gateway (runtime and proxy-shaped access lines),
billing-worker, acme-storefront (build lines, ANSI coloured and stripped) and
docs (proxy access) — plus system and agent lines. The story is the one the
rest of the console tells: `dep_31c` ships to billing-worker at 19:30 and its
health check never passes again, so 41 errors in the last hour, 38 of them
from billing-worker, and the other three are the 502 on docs, the TypeError
in AddressForm and the failed staging build. A handful of JSON lines and a
handful of ANSI ones are in the set on purpose, because both have to render.

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

The landing header carries Temps Cloud's own verdict at its right end, as
a link to `/status?project=temps` (the page alone, no sandbox chrome):
`Status` glyph + word, `● all systems operational`, `× API down` or
`◐ 2 components degraded`, computed by the same `statusSummary(project)`
the page uses, so the indicator and the page cannot disagree. The
storefront page keeps the incident scenario; the platform's page is quiet,
because a marketing page pointing at a live outage is a different design
problem. The whole cell is the link; below `sm` the word shortens to
`operational` and the star count, which is decoration, gives it the room. It is the one place on the landing a state tone
appears above the fold, which is what makes it readable as a verdict and
not decoration.

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

First run (`/v1?p=analytics&fresh=1`): the
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

- First run (`/v1?p=email&fresh=1`): every tab onboards instead of going
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
  quoting the agent's last error when offline. Content is the Resources
  entry below: four charts on one axis, the containers that made them, and
  an aside of Gauge tiles, the machine facts and the three actions (drain,
  restart agent, add a node). Two facets and no third — resources (the
  machine) and agent log (what it has been telling us); containers folded
  under the charts, because the ledger is the attribution the charts owe.
- **Cluster** is a settings page: joining (the token as a SecretValue with
  regenerate, the three commands to run on the machine), cluster dns (the
  toggle, the locked pool and prefix with why they are locked), trust (the
  CA fingerprint), and CA rotation in the danger zone with what it breaks.
  The list's hint links there and "join a node" goes there.
- Every "hetzner-1" mentioned on a database or deployment record opens
  `node:hetzner-1`; before this the link went nowhere.

### Resources (`node:<name>` · the project record's `resources` section)

`src/sections/ConsoleV1Resources.tsx`. Dokploy's monitoring page is four
gauges and four charts in a row of cards with a range picker on top: it tells
the operator that memory is at 91% and stops there, which is the half of the
sentence they cannot act on. Ours is a record, not a dashboard.

- **The verdict is the page.** `◐ Memory is at 92% of 8.0 GiB and has been
  climbing for two hours, since dep_31c. billing-worker-dep_31c-1 holds
  1.4 GiB of it and has restarted 2 times. Move it to another node, or add a
  node.` Every phrase is a link: the deploy that caused it, the container that
  holds it, the page that adds capacity. A healthy machine still gets a
  sentence with the fact that proves it — "Nothing to do: cpu, memory, disk and
  network are all under their warn lines, and the busiest is disk at 78%. At
  400 MB a day the disk is full in 44 d." A number with no verb is a
  dashboard; a verb with no number is a slogan.
- **The lede is the four resources plus who is running and who is talking**:
  cpu (now · peak with its time), memory (used *of* total *and* the share),
  disk free (with the projection), network (in · out), containers, heartbeat
  (with the agent version). Six facts, each of which the reader would
  otherwise have to hunt for; the meta carries `worker · fsn1 · direct` and
  nothing else, because a fact appears once.
- **One axis, one cursor.** The four charts are the same 48 buckets of 30
  minutes, and hovering 19:30 on any of them reads 19:30 on all four —
  `cpu 20% · memory 38% (3.1 GiB) · disk 22% · network 4.0 MB/s in`. That is
  the whole point of the page: "what was cpu doing when memory climbed" is one
  question, and four charts with four independent hovers make the reader ask
  it four times and hold the answers in their head. A `cursor` control above
  the strip is the keyboard's way in (`←` `→` walk the buckets, `esc` clears),
  and the bucket is announced in a live region. The cursor itself is a dotted
  ink rule drawn over all four plots at the same fraction of the axis.
- **A threshold is a line with a word, and tone is only where it crossed.**
  Each chart carries its two dashed threshold lines labelled in their own
  words — `busy 80%` / `saturated 95%`, `tight 85%` / `oom risk 95%`,
  `tight 80%` / `writes stop 90%`. The series stays ink for the whole window;
  a second series carries **only** the buckets at or above the line, drawn on
  top in the threshold's tone and kept out of the table view (it is the same
  numbers as the line it marks). The footer states the excursion as a fact:
  `◐ above tight for 1 bucket (30m 00s)`. Toning the whole line, as the first
  cut did, says memory was bad all day when it was fine until 19:30.
- **A percentage always arrives with its absolute.** The header says the unit
  once (`memory · % of 8.0 GiB`), the readout says `92% (7.4 GiB)`, the gauge
  says `7.4 GiB of 8.0 GiB`. Disk is drawn as a share rather than in GB for
  one reason: the axis is then bounded 0–100 and its two threshold lines are
  always on it, however empty the disk is. The absolutes ride the readout and
  the footer.
- **Disk states its projection.** The dashed second series is the fitted trend
  at the measured growth rate, and the footer names where it lands and when:
  `125 GB free · at 100 MB/day it hits the 90% line on Aug 28 2029 and is full
  on Feb 4 2030`. A disk is the one resource whose future is knowable, so not
  saying it is a choice to withhold it. The projection is drawn on the shared
  axis, not on an axis of its own: the picture stays comparable with the other
  three, and the date does the extrapolating.
- **Pressure is attributed.** Under the charts is one `Ledger` of the
  containers on the machine — kind icon, name, cpu, memory *of its own limit*
  with the share, network, restarts, uptime, and a `Sparkline` each for cpu
  and memory — sorted failing-first then closest to its own limit, `⏎` opening
  the service. A chart that says 92% and does not say which container holds it
  has told the reader nothing they can do. The memory chart's `total · by
  container` segment swaps the line for a `StackedInk` of the three biggest
  containers plus an honest remainder, so the layer that grew after the deploy
  is visible as a layer.
- **The aside is what is left**: four `Gauge` tiles (current, peak with its
  window, thresholds as ticks carrying their words), "what is using it" as the
  top three by memory, the machine facts the lede does not carry (arch,
  kernel, docker, disk device), and the three actions — `drain node`
  (`EchoDialog`, typed, because it moves containers), `restart agent`
  (`EchoDialog`, ink not red, because it is reversible and nothing is
  redeployed), `add a node`. The agent version is a lede fact and therefore
  *not* in the aside.
- **One reversible click makes the monitor.** In the memory chart's footer:
  `alert when memory > 90% for 10m`. It creates the monitor, toasts, and turns
  into `◐ alerting when memory > 90% for 10m · undo`. The moment a reader
  learns a threshold matters is the moment to offer the alert; sending them to
  a settings page loses them.
- **Live is honest.** A `Live` control polls every 30s and **pauses itself the
  moment the reader scrolls**, saying `paused`; the same control resumes, and
  `space` toggles it. A number that moves under somebody reading it is worse
  than a number that is a minute old. On an offline machine the control is not
  drawn at all — it says `○ not live · no samples since 21:25`, because
  nothing is arriving and a spinning "live" badge would be a lie.
- **A node with no samples keeps its tiles.** hetzner-3 is offline: the record
  keeps every tile, every plot and every row, greys them, and stamps each with
  the time it was last true (`of 4 vCPU · last true 21:25`), over a `Callout`
  quoting the agent's last error verbatim. An empty page is indistinguishable
  from a healthy one, and this machine is neither.
- **The range strip** is `1h · 24h · 7d · 30d` with everything past the sample
  horizon struck through and explained on click, never hidden. Node samples
  are kept 7d — a different shelf from the plan's 30d telemetry retention, and
  every footer says which one it is quoting.
- **Phone (390).** The four charts stack, each keeping its readout, footer and
  table view; the aside collapses behind a `details` button at the *top* of
  the column (a `Drop` opening downwards — anchored at the foot of a long page
  it would open off-screen); the containers ledger renders its mobile row,
  `name · memory of limit`, with the state glyph in its own slot and the row
  itself as the action. No horizontal document scroll at 390 or 1440; the
  ledger is the one deliberate sideways scroller.
- **The service side.** The project record already carries six tabs and a page
  gets one row of them, ever — so a service's resources are a `Section` inside
  `overview`, between the request chart and the deploys, not a seventh tab.
  The same four charts, the replicas summed, the same shared cursor; then the
  per-replica ledger with the node each one runs on as a link, because "the
  service is fine but one replica is on the machine that is not" is the thing
  this view exists to show. A stateless service has no disk of its own, so the
  fourth chart is a `PageState` that says *which* of the four reasons it is —
  "no volume", not "no data" — and links the nodes where the bytes actually
  land.
- **The nodes ledger** gained four `Sparkline`s per row (cpu · memory · disk ·
  network, the same 24h the record plots) and a pressure-first default order:
  unreachable first, then closest to a threshold. The phone row carries the
  one fact a phone has room for — the worst thing on that machine right now
  (`memory 92%`, or `no heartbeat for 4m · last sample 21:25`).

**The fixture.** Deterministic: a seeded PRNG (mulberry32) and a clock frozen
at 2026-09-06 21:29 UTC. No `Math.random`, no `new Date()` for a value, so the
same 48 buckets render on every reload and a visual baseline means something.
The story it tells: hetzner-2 sits at 38% memory all day until `dep_31c` lands
billing-worker at 19:30, which leaks from 420 MiB to 1.4 GiB and takes the
machine to 92% over the next two hours; hetzner-1 is a control plane whose cpu
spikes to 83% three times while it builds and whose disk is at 78%, growing
400 MB a day; `dep_91a` at 20:34 bursts the network on both while the image is
pulled; hetzner-3 has been silent for four minutes and shows the last values
that were true at 21:25. Fictional names throughout, and addresses only from
the documentation ranges (`10.0.3.x`, `203.0.113.x`).

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
`src/sections/AgentChat.tsx`. Every block is now a primitive in
`@temps-sdk/op` (`agent.tsx`, §6), and the whole rule set is
`docs/generative-ui.md`.

**Two scenarios, one ledger**, chosen with a `Segmented` in the page header,
because the claim the surface has to survive is that an agent writing code and
an agent answering an operator are the same kind of record:

- **Coding agent** (`fix: address form null id` · `api-gateway · worktree
  feat/checkout-address`). A `TypeError` in `AddressForm` since `dep_91a`: find
  the cause, fix it with a test, run the suite, open a PR, and do not touch the
  Stripe retry code. It is the surface for everything a run *does* — reasoning
  that collapses to "thought for 6s · 3/3 steps", reads and greps that collapse,
  an edit that opens with its unified diff, a command open with its output, one
  failing command shown as `× failed` with the error verbatim, a subagent
  holding its own transcript, tasks, a checkpoint, a plan, and the push waiting
  on an inline approval. Its aside carries tasks, files changed and permissions;
  its permission mode is "accept file edits", flagged `warn` because it is wider
  than the default.
- **Console assistant** (`why did checkout errors jump?` · `checkout-web ·
  production`). The same ledger answering an operator with the console's own
  blocks: `get_error_time_series` produces a `TimeChart` and
  `list_error_groups` produces a `Ledger`, each wrapped in `Provenance` with
  `show query`, so the reader can always get from a picture back to the call
  that drew it. The verdict comes first in words ("Roll back checkout-web
  production to dep_90c"), and the one write is a `Proposal` — action, target,
  consequence, reversibility, autonomy — that does nothing until it is
  confirmed; declining it says what did not change ("Nothing ran. Production
  stays on dep_91a at 9.1 errors per minute"). Its aside is the run plus the
  autonomy list per capability, ending in `delete anything · not on the
  allowlist`, because what is *not* allowed is a fact the reader needs before
  they ask. It is read-only: writes propose.

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

- The shell is sticky and the main column is the only thing that scrolls. The
  sidebar rail and the header pin (`sticky`, `self-start`, `top-0` full-screen
  and `top-12` under the sandbox chrome, each with its own `overflow-y-auto`),
  and the `Inspector` pins the same way, with its own scroll and its `top`
  aligned to the bottom of the header. A shell that scrolls away takes the
  navigation *and the attention badge* with it, so on a long screen — Logs is
  the one that exposed it — the reader has to scroll back to the top to find
  out that anything is wrong. Below lg the sidebar is a fixed drawer and none
  of this applies.
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
| `esc`       | everywhere          | close drawer, menu, dialog                |

Keys are ignored while an input has focus. Every key has a visible badge —
that is the rule, not a nicety: a keyboard shortcut is an accelerator for a
control the reader can see, never the only way to reach a behaviour. Density
used to have a `d` key and a header button; both are gone, because density is
a design rule (row heights, the `dense` prop) rather than something the
console asks the operator to decide.

**The cursor is the focus.** In a ledger, `j`, `k` and the arrows move DOM
focus to the row they mark, and the row's own key handler opens it on `⏎`.
The window handler only opens the cursor row on `⏎` when nothing is focused.
A cursor that merely paints a bar while focus sits on a tab or a footer link
makes `⏎` act on that other element: the reader sees a marked row, presses
enter, and lands somewhere else. Any new list with a cursor follows the same
rule: never move a highlight without moving focus with it.

## 10. Plans as design input

The plan is a design input, not a control. `ConsoleV1.tsx` fixes it in one
constant, `const PLAN: PlanId = 'starter'`, read through `usePlan()`; the
header carries no plan switcher, because a plan is not something an operator
flips from a toolbar. Starter is the value chosen because it keeps every
plan-dependent behaviour on screen at once: 30d retention (so the 90d and
13mo ranges render struck through), a 10 GB allowance the demo data has
already passed (so charts, status lines and footers say `sampled`), and a 7d
PITR horizon in the databases ledger. Change the constant to see the same
screens on another plan. In the real console the value comes from the license
or Cloud subscription. What changes per plan:

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

What exists today, in `bun run lint`: `tsc --noEmit`, `scripts/audit-records.mjs`
(the eight record rules), and `node ../web/packages/op/scripts/tokens.mjs check`
(`tokens.json` against `op.css`, value by value and name by name, in order).
The token check is the first of these that guards a token rather than a
structure, and it fails with a printed diff rather than a count.

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
    forms.md                      field anatomy, validation timing, saving, secrets (/guide#forms)
    notifications.md              which surface says it: verdict, callout, toast, bell, dialog
    content.md                    the words: capitalisation, terms, errors, buttons, numbers
    localisation.md               expansion, no concatenation, logical properties, RTL readiness
    data-viz.md                   which chart answers which question; series without a hue
    generative-ui.md              what an agent may render: the console's blocks, provenance, proposals (/guide#generative-ui)
    motion.md                     the three duration tokens, what may move, the two exceptions
    icons.md                      lucide only, two sizes, the concept → icon vocabulary
    design-system-answers.md      the twelve questions, answered
    operator-console-brief.md     original brief (historical, do not edit)
  src/
    globals.css                   all tokens; blocks listed in §4
    components/op/                the operator library (§6, §7)
      index.ts  kbd.tsx  status.tsx  num.tsx  page-state.tsx
      echo-dialog.tsx  templates.tsx  time-chart.tsx  picker.tsx
      fmt.ts    the formatters content.md's number rules live in
      form.tsx  FormErrors, the multi-field submit summary
      datetime.tsx  date, time, range, duration and schedule fields, and the
                    Strip that RangePicker shares with them (forms.md)
    ../web/packages/op/src/       the package itself; the files above re-export it
      agent.tsx     ToolRow, Proposal, Provenance, StreamBlock, AgentQuestion, AgentSources, RunAside (generative-ui.md)
      inspector.tsx the panel that reads one ledger row beside the list, following its cursor (§6, §7)
      viz-ink.tsx   the shared ink vocabulary every figure is built from: Figure, DataTable, InkPatterns, StateWord, the readout, the five density steps
      viz-time.tsx  BandChart, StackedInk, LatencyHeatmap, StateTimeline, WindowTimeline, SessionTimeline
      viz-grid.tsx  PercentileLadder, CohortGrid, DeltaTable — the figures that are tables first
      viz-graph.tsx PathTree and Topology, each with the same nodes as a list beneath it
      viz-usage.tsx UsageBar and Gauge: a measure against an allowance, and a machine's pressure
    ../web/packages/op/tokens.json    the token layer as data (§4)
    ../web/packages/op/scripts/tokens.mjs  check / build, wired into bun run lint
    components/ui/                shadcn primitives + sparkline, log-viewer, empty-placeholder
    components/platform-logos.tsx, system-map-section.tsx
                                  copied verbatim from temps-landing; do not edit here
    sections/ConsoleV1.tsx        reference console (uses components/op)
    sections/ConsoleV1Env.tsx     deploys + promote, environments, variables tabs (§7b)
    sections/ConsoleV1Observe.tsx sandboxes, sandbox detail, traces, trace waterfall, metrics (§7b)
    sections/ConsoleV1Logs.tsx    the Logs tool screen and the log record (§7b)
    sections/InkLandingV1.tsx     reference landing
    sections/Guide.tsx            /guide — renders docs/*.md into one consolidated page
    lib/md.ts                     the slicing helpers the guide cuts documents with
    sections/OpComponents.tsx     component reference page
    sections/blocks/              the live half of the documents above:
      FormBlocks NotificationBlocks ContentBlocks DataVizBlocks TokenBlocks
      DataVizBlocks2  the second wave of figures, viz-band … viz-topology (data-viz.md §§9–23)
      GenUiBlocks     tool rows, provenance, proposals, streaming, questions (generative-ui.md)
      (mounted by both /guide and /op-components — one implementation)
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
9. Four primitives the Logs screen (§7b) wanted and had to hand-build, in the
   order they cost the most:
   - **`LedgerRow.expanded?: ReactNode`**, rendered as a full-width subgrid
     row. `.op-row` is a fixed `--row-h` on desktop by design, so a row cannot
     grow to hold its own detail and `e` opens the expansion in a `Section`
     under the list instead of in place. The same slot would serve the proxy
     access log and the deploy checks.
   - **`QueryBar({ keys, values, tokens, onTokens })` in the package**, with
     the shared `key:value` grammar (`writeQuery` / `readQuery`) so every tool
     screen serialises the same way. The Logs query bar is about 90 hand-built
     lines — chips, the typed suggestion `Drop`, the arrow-key cursor, the
     `backspace`-removes-last rule — that traces, proxy logs, the audit log
     and analytics events will each need identically.
   - **`BreakdownRow.of?: { count: number; label: string }`**, a second
     measure on a facet row. "Count, and how much of it is errors" is the
     facet question, and the second bar currently has to be drawn inside the
     row `label`.
   - **`Picker` multi-select**, which is why two projects arrive through the
     facets and are described back to the Picker as a synthetic option
     ("in api-gateway, billing-worker · from the facets").
   Minor, with it: `Drop` anchors right by default, so a suggestion panel
   needs a `start-0 end-auto` override.
10. Requirements (`docs/requirements.md`) are enforced by `e2e/state.spec.ts`
    (reload signatures) and nothing else. The lint has no rule for "a `useState`
    that decides what is on screen", and three axes are still local by choice
    and want a second look: the agent conversation, the Logs live tail, and the
    `Settings` form drafts, which are the one case where the address cannot
    carry the state and the page has to say so instead.
11. Nine gaps the Resources screens (§7b) hit and worked around locally,
    with none of the package touched. The first two are the expensive ones:
    - **`TimeChart` has no `cursor` / `onCursor` pair.** It owns its hover
      state, so a group of charts cannot be made to read the same bucket.
      Worked around with a local `ChartStack` + `AxisPane` holding one bucket
      index and feeding every `readoutFormat`. The visible seam: the word
      beside each readout (`hover` / `latest`) still comes from `TimeChart`'s
      own state, so a chart the pointer is not over says `latest` while
      showing the shared bucket. `cursor?: number | null` +
      `onCursor?: (i: number) => void` removes the wrapper and the seam.
    - **`TimeChart` cannot pin its y domain.** `yTicks` sets ticks and the
      domain stays recharts' `[0, auto]`, so a threshold above the data's
      maximum is simply not drawn. Disk had to be re-expressed as a
      percentage, and the service memory chart needs `yTicks` reaching the
      limit for the limit line to appear. `yDomain?: [number, number]`, or
      extending the domain to cover `thresholds`, is the fix.
    - **`TimeChart` has no partial-bucket hatch.** `StackedInk` takes
      `partial`; `TimeChart` does not, so these footers say "current bucket
      partial" without the picture saying it.
    - **`StackedInk` cannot join a shared cursor** either — it owns its own
      readout region, so memory's `by container` view reads independently of
      the other three.
    - **`RangePicker` cannot be asked for the strip alone.** It bundles the
      custom-window popover, which a phone does not want here, so the gated
      strike-through had to be rebuilt locally (`Ranges`). A
      `custom={false}` variant, or exporting the strip mapping, would do.
    - **`Gauge` has no stale rendering** between healthy and `idle`, and
      `idle` empties the bar — wrong for a node whose last sample is four
      minutes old and still true. Worked around with muted ink and the stamp
      in `of`; a `stale?: ReactNode` that greys the fill would be better.
    - **`Live` draws a `space` `Kbd` badge and binds no key.** Every caller
      has to add the window listener, and every caller will forget. The
      binding belongs beside the badge.
    - **The generated legend prints `92 %` and `1,434 MiB`** through `fmtNum`
      plus a space, where `content.md` §6 asks for `92%`. A
      `format?: (n: number) => string` on `TimeChart` would let the legend,
      the readout and the table spell a number the same way.
    - **Threshold labels overprint** each other and the deploy-cluster label
      at the right edge when two lines are close (`busy 80%` under
      `saturated 95%`).
12. The `op.css` blanket transition rule carries `!important`, so every class
    that must actually animate has to be lifted out of it with a `:not(…)`
    arm — now `.animate-spin`, `.animate-pulse` and `.op-pulse`. That is
    fragile (opting one class in means editing a selector three hundred lines
    away from the class itself) and it collides with the dialog's own
    `--op-duration-slow`. Replace the blanket `!important` with a scoped rule
    so opting in stops requiring a remote edit.

Found by the kitchen-sink stress page (`/kitchen-sink`), not yet fixed:

- Ledger, Detail and the console shell bind keyboard handlers to `window`. Fine
  with one screen mounted; on a reference page with several ledgers `j`/`k`
  move every cursor. Scope handlers to focus-within before the real console
  mounts more than one template per route.
- ~~Field has no error slot; an invalid input's message goes in `help`, which
  reads as advice.~~ Done: `Field` has `error`, `hint`, `optional` and `id`, and
  `FormErrors` summarises a multi-field failure (§6, `docs/forms.md`). Settings
  still does not block save while an error is set — and should not: a save
  button disabled because the form is invalid explains nothing (`docs/forms.md`).
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

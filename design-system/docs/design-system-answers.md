# The design-system questions, answered for Temps

Companion to `brand-guidelines.md`. Sources: `temps-landing/public/pricing.md`
(2026-08-30), `temps/CLAUDE.md`, and a survey of `temps/web` (117 page files,
516 tsx). Numbers are from the survey, not estimates.

## 1. Who is it for, and what are they doing?

Pricing tells us who pays and who doesn't:

- Self-hosted, $0, unlimited users. Indie hackers and small teams on a $5–10 VPS.
  They are alone; there is no support channel. The console is their only help.
- Cloud Starter $29 and Team $99. Startups replacing $500+/mo of observability.
  The buyer is the same person who operates it.
- Business $299 with 13-month retention and a spend cap. A team lead who has to
  justify the bill and prove things happened.
- Enterprise with SSO, SLA, compliance. A platform team plus an auditor.

So the primary reader is an operator debugging their own box, usually at a bad
hour, with nobody to ask. The secondary reader is that same operator justifying
Temps to a team. The landing page talks to the second one; the console to the
first. When they conflict, the console wins: nothing in the console exists to
impress.

The roadmap adds a third reader: the agent. Temps is becoming a system that
observes, proposes and (under policy) acts on the operator's behalf, with
skills, MCP servers and scheduled agents as first-class objects. The agent is
held to the operator's rules: typed tool calls in a ledger, evidence before
adjectives, proposals with an inline approval, an autonomy level in words. See
brand-guidelines §0 and `/agent`.

The emotional job is "I can see what is wrong and what to do". Not "this looks
modern". Pricing reinforces this: it promises the console will say when
telemetry is sampled rather than drop it silently. That promise is a design rule.

## 2. What should a stranger recognise?

Today: nothing. The console is Geist plus stock shadcn, and `DESIGN.md` says so.
A cropped screenshot could be Vercel's.

Chosen signature, in order of importance:

1. The verdict: one sentence per page, kept behind a glyph + count in the header and opened on demand (the inline status line remains for pages without a shell).
2. Deploy markers on every time axis. Temps is the only tool that has both the
   deploy and the metric, so this is the move competitors cannot copy.
3. Paper and ink: warm paper, ink borders, one raised element per screen.
4. Typed confirmation with a copyable resource badge on destructive dialogs, and key badges on primary actions.

The first two are data patterns, not styling. That is deliberate: a colour can be
copied in an afternoon; "every chart knows when you deployed" cannot.

## 3. What are we refusing to do?

Banned, and to be enforced in CI:

- Tailwind palette literals (`text-red-500`, `bg-slate-50`). Current count:
  2,265 in 189 files. The ratchet fails a PR that raises the number.
- Hex and raw oklch in tsx. Current count: 129.
- A second hue. Colour is status or the single accent on `--primary`.
- Spinners as page state. `Loader2` is in 134 files, `Skeleton` in 141. Skeleton
  for loading, spinner only inside a button that was pressed.
- Silent empty states. Pricing promises "the console says so"; an empty chart
  must say why (no data, sampled, quota, not configured), never just render nothing.

Banned by taste, doc only: cards as layout (214 files use `Card`; 52 use
`Table`), gradients, decorative icons, motion longer than 100ms.

## 4. What are the axes, and what is fixed?

Axes we ship:

- Light and dark. Already in 124 files via `dark:`; keep.
- Density: default and dense. Operators with 40 projects need the dense row.
- Platform for key badges: ⌘ vs Ctrl.

Axis we do not ship: accent. Decide `signal` or none and freeze it. A user
picking a hue is a Cloud feature nobody asked for.

Fixed: Geist and Geist Mono, 1px ink borders, radius 0.25rem in the console
(today 0.5rem), 8px spacing grid, tabular numerals everywhere.

## 5. What is the hierarchy model?

Weight, then size, then position. Six tiers from `brand-guidelines.md`:
`op-display` 800 (landing only), `op-h1` 700, `op-h2` 600, `op-h3` 600,
`op-lead` 400 muted, `op-label` 500 uppercase. The console's largest tier is
`op-h2`. A console page has: status line, one `op-h2`, panels with `op-label`
headers. Nothing else.

## 6. What is colour for?

Status only: ok green, warn amber, error red, always next to a glyph and a word,
never alone. Focus ring blue on focus only. Links are ink with an underline.
The single accent, if adopted, lives on `--primary` and appears on the primary
action once per viewport. Charts are ink on paper; the deploy marker is a
dotted ink line, the incident window is a muted band.

Pricing gives a fourth status: sampled. Telemetry past the allowance is
head-sampled "and the console says so". That state needs its own glyph and
word, shown on the chart and in the status line, not a toast.

## 7. What does data look like?

Temps sells observability, so this is the core of the system, and today it is
the least specified part. Rules:

- Numbers are mono and tabular. Units follow the number with a thin space.
  `30.8k`, `184ms`, `0.61%`, `99.94%`. Never spelled out in prose.
- Time is relative under a day (`41m ago`), absolute after, always with a
  deploy id next to it when one exists.
- Every time axis has deploy markers. Every metric tile says what it is compared
  to (`+9% since dep_91a`), never a bare delta.
- Empty value is an en dash. Zero is `0`, not blank.
- A chart with no data says which of four reasons: no traffic, not configured,
  sampled past quota, retention expired. Retention differs per plan (30/90 days,
  13 months), so the console must state the horizon on the axis.
- Logs are a viewer with a gutter, level colour, and follow toggle. Not a `<pre>`.

## 8. What does failure look like?

One pattern, four states, in one component: loading (skeleton), empty (reason
plus next step), unconfigured (what is missing, example of what it would show,
link to the settings page), error (message, resource, retry). Today there are
three empty-state implementations and `EmptyState` is used in 44 files. Merge
them; the merged one gets the `steps` prop from the sandbox's `EmptyPlaceholder`.

The self-hosted user has no support. A failure that requires a restart to
notice is a design failure, per `CLAUDE.md`.

## 9. How does someone build a new screen without asking?

Today: start from a blank page and copy a neighbour. That is why the 117 pages
drift. Fix: three page templates in `web/src/components/page/`.

- Ledger: status line, filter, table, footer with counts and keys. Projects,
  deploys, domains, users, backups.
- Detail: status line, tabs with number keys, one chart with markers, stats row,
  one raised incident or activity thread. Project, service, database.
- Settings: form sections, sticky save with ⌘S, danger zone with typed confirmation.
  The 15 settings pages already share this shape informally.

Five components cover most screens: `Table` with density, `StatusLine`,
`Sparkline`/`TimeChart` with markers, `EmptyPlaceholder`, `Kbd`.

## 10. How does it stay true?

- Lint: a script counting palette and hex literals, run in CI, fails on
  increase. Number goes in the PR check output.
- Visual: the design-system app's `/v5` and `/brand` are the reference. A
  Playwright screenshot diff on those routes per PR. The console has no Playwright
  today; this is the first use.
- Ownership: the design-system app and `docs/brand-guidelines.md` are the
  source. `web/DESIGN.md` gets a pointer and stops being edited separately.
- Change process: a rule changes by editing the doc and the reference page in
  the same PR. No rule lives only in someone's head.
- Agents read the same file: add a line to `temps/web/CLAUDE.md` pointing at the
  guidelines and the banned list.

## 11. What is the migration path?

First screen: the Projects ledger, then the Project detail. They are the two
most visited pages, they exercise all five components, and they are what a
trial user sees in the first five minutes of the 14-day Cloud trial.

Mechanics:

1. Land the ink tokens behind a `.operator.ink` class on the shell. Nothing
   changes until the class is set.
2. Codemod palette literals to tokens (`text-red-500` to `text-destructive`,
   `bg-slate-50` to `bg-muted`). Most of the 2,265 are mechanical.
3. Ship the two screens on the templates. Enable the class by default.
4. Ratchet the rest: every touched file must reach zero literals.

Done means: the ratchet count is zero, the three templates exist, and the two
reference screens match `/v5` in the screenshot diff.

## 12. What would we regret?

- Radius. Moving 0.5rem to 0.25rem touches every control. Test on the real
  Settings pages before committing, not on demos.
- Border colour. Ink borders on every input are stronger than shadcn's grey;
  dense forms may look heavy. The 15 settings pages are the test.
- The accent. If it ships, it is in every screenshot forever. Decide once.
- Density as default. Operators like it; first-run users on the trial may not.
  Default to comfortable, remember the choice, and show the `d` key.

## Reference implementation

`/v5` in the design-system app (`src/sections/ConsoleV5.tsx`) is these answers
as code: `Ledger`, `Detail` and `Settings` templates, one `PageState` with four
states, the `sampled` status, retention horizon on the chart, `Metric` tiles
that require a baseline, density remembered, no accent axis. `/v5-landing` is
the v3 landing plus pricing from `pricing.md`, the limits table, a mobile menu
and the accent frozen to `signal` on the primary CTA.

## The three to answer this week

3 (the banned list, as a CI check), 10 (the ratchet), 11 (Projects ledger on the
new template). The rest is drafted here or in `brand-guidelines.md`.

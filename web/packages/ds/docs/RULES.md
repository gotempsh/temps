# @temps-sdk/ds — RULES

Imperative digest. If a rule and a screenshot disagree, fix the screenshot.

## Setup

- Depend on `@temps-sdk/ds` (workspace) and import from it, not from `web/src`
  directly, for anything the package exports.
- Never import `.tds` scoping into `:root`. Add the class to the subtree that
  opts in (the sandbox's `<body>` does this). It must never fight the host
  app's own theme.
- Base primitives (Button, Badge, Input, Card, Table, Command, AlertDialog…)
  come from `@temps-sdk/ui`. Do not re-import shadcn separately.

## Non-negotiable

- Every screen is built from a template (`Ledger`, `Detail`, `Settings`) or a
  primitive on top of one. Hand-rolled `flex items-center justify-between`
  page headers are a bug, not a style choice.
- An unconfigured feature always renders a surface: what's missing, a
  concrete example, a link to the settings page. Never nothing. `PageState`
  variant `not-set-up` enforces this at the type level — `requirement`,
  `example`, `settingsHref` are required props.
- Color never decorates. It appears only through `Status` (badges, dots,
  chart series). If you reach for a raw color to convey meaning outside
  `Status`, stop and add a tone to `Status` instead.

## Tokens

- Colors, radius, spacing, type, shadows: `tokens.json`, consumed via
  Tailwind's semantic classes (`bg-success`, `text-muted-foreground`,
  `rounded-md`) — never a literal.
- `bun run lint` runs `tokens:check`, which fails if `tokens.json`,
  `src/tokens.css`, and `web/src/globals.css` disagree.

## Banned

- Raw hex (`#0070f3`) or `oklch(...)` literals outside `tokens.json`/`src/tokens.css`.
- Raw `px`/`ms` literals in components — use the Tailwind scale or a token.
  `audit-records.mjs` catches both; `// audit-ignore` requires a reason.
- `disabled` on a button mid-action — use `Button`'s `busy`/`busyLabel`.
- Toasts for confirmation of a click the user is still looking at
  (`CopyAction` answers on itself). Toasts are for events the user isn't
  currently watching.

## Type

Geist / Geist Mono (unchanged tokens). One `h1` per page, via `PageHeader`.
Never a second `h1` on the same route.

## Motion

Reuse the utilities already in `globals.css` (`animate-in`, `fade-in-50`,
`animate-spin`, etc.) — do not hand-write a new keyframe for something those
already cover. Respect `prefers-reduced-motion` (already handled globally).

## Status vocabulary

Five tones only: `ok` `warn` `error` `idle` `running`. Dot + word, in a Badge
by default (`variant="dot"` for dense rows). Never invent a sixth tone or a
glyph vocabulary — extend `STATUS_TONES` in `status.tsx` and update this file
in the same change if the app genuinely distinguishes a new state.

## Icons

`lucide-react` only, sized via the Tailwind scale (`size-4`, `size-10`), never
a raw `width`/`height`. Icons pair with text/Status — they never carry
meaning alone.

## Page structure

`PageContainer` owns horizontal padding — nothing else in a page tree sets
`px-*` on its outer wrapper. `PageHeader` owns the `h1` + actions row.

## Record page checklist

Title → verdict (`Status`) → 4-6 facts → main column → aside. If you have
more than 6 facts, the rest belongs in `main`, not the fact grid. `aside` is
optional; `main` is not.

## Forms

`Field` for every control (label + control + description + error, wired
`aria-describedby`). `FormErrors` above the sticky save bar, not only inline.
`Settings`'s save bar is always mounted — it doesn't appear/disappear with
`dirty`, so its position never jumps.

## Notifications

In-page state (validation, "not set up", empty) → `Callout`/`PageState`.
Confirmation of the user's own click → the control itself (`CopyAction`).
Background events the user isn't watching → `sonner` toast (unchanged,
outside this package's scope).

## Data

Numbers, bytes, durations, relative/absolute time → `fmt.ts`. No hand-written
`toFixed`/`Intl` calls scattered across screens.

## Collection states

Keep the section heading and filter controls mounted while data changes.

- First request pending: `DataTable isLoading`, with an accessible table name
  (`aria-label`). Keep the real column headings; never show empty copy yet.
- Successful response with no records: compact `PageState` `empty`, explaining
  what creates the first record.
- No filter matches: compact `PageState` `empty`, naming the active filter and
  offering a working clear-filter action. Do not imply no records exist.
- Request failed with no usable data: compact `PageState` `failed`, naming what
  could not load and providing a retry of that request.
- Background refresh failed: keep the last usable rows and filters visible;
  add a `Callout` explaining the data may be stale, with a retry action.
- Multiple independent requests: one panel's failure must not erase another
  panel's usable content. Use separate query states.

Keep errors, empty content, and loading in the collection's usual location.
Do not nest a second bordered empty-state panel inside a table surface.
`DataTable` owns its border and horizontal scroll; `PageState` can replace
it or sit inside an existing surface without adding another border.

Example: sandbox `/table-states` has shareable scenarios, working retries,
and filtering. Production reference: `CronJobDetail.tsx`.

## Charts

`TimeChart` (wraps `ThresholdLineChart`) for every time series. Don't hand-roll
a new recharts panel — extend `TimeChart`'s props if it's missing something.

## Content

No vanity data, no placeholder customer names/hostnames in fixtures (repo-wide
rule — see the worktree's `CLAUDE.md`). Empty and not-set-up copy states the
real requirement, not "Nothing here yet."

## Keyboard

Shortcuts get a visible `Kbd` badge next to the control they trigger, never a
keyboard-only entry point. `Picker` is filterable by keyboard from focus.

## Responsive

`PageHeader` actions wrap to a second row before they overflow. `Detail`'s
aside stacks below `main` under `lg`. Tables scroll horizontally before they
break layout — never let a fact grid or button row force horizontal page
scroll.

## Requirements: the URL is the state

Filters, tab, page number, selected record: `useUrlState`, not local
`useState`. A refreshed or shared link must reproduce what the user saw.

## Before you ship

1. `bun run lint` passes (typecheck + tokens:check + audit:records).
2. Every "not set up" surface names the missing config and links to it.
3. No raw color/px/ms literal outside tokens — the audit catches most of
   this, but re-scan by eye for anything the regex can't (inline `<style>`,
   a `boxShadow` object).
4. The record recipe order (title → verdict → facts → main → aside) holds.

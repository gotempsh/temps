---
name: temps-design-system
description: >
  Build or review console UI so it reads as Temps: the paper-and-ink operator
  design system (`@temps-sdk/op` primitives, the `operator ink v1` skin,
  the Ledger / Detail / Settings page templates, the status vocabulary and
  the record recipe). Invoke when a task adds or redesigns a console screen,
  a landing section or a status page on the new design system, when the user
  says "follow the design system", "make it look like temps", "brand
  guidelines", "taste", "op components", or when reviewing a UI PR against
  the guidelines. Not for the legacy `web/src` console: that stays on its
  current shadcn look until it is migrated screen by screen.
---

# Temps design system

The design system is a sandbox app plus a component package. Everything a UI
task needs is in the repo; do not invent tokens, colours or page shapes.

| What | Where |
|---|---|
| Rules digest for agents (read first, imperative, short) | `design-system/docs/RULES.md` |
| Brand guidelines (why the rules exist) | `design-system/docs/brand-guidelines.md` |
| Handoff: tokens, primitive catalogue, page templates, responsive, keyboard | `design-system/docs/design-system-handoff.md` |
| Component package consumed by screens | `web/packages/op` (`@temps-sdk/op`) |
| Consumer setup (a plugin, an outside app): install, `op.css`, `@source`, the skin class, fonts | `web/packages/op/README.md` |
| Reference implementation of every screen | `design-system/src/sections/ConsoleV1*.tsx` |
| Browsable guide, component gallery, console mockups | `cd design-system && bun install && bun run dev` → `/guide`, `/op-components`, `/v1` |

## Scope boundary

What exists today, stated plainly so nobody assumes more: **the production
console (`web/src`, rsbuild) does not import `@temps-sdk/op` yet.** What is
built is the system (the docs), the package, and the sandbox that renders every
primitive and every screen shape against it. Console migration happens screen by
screen, on a schedule, not as a side effect of another task.

- **Redesign work** (new screens on `@temps-sdk/op`, the sandbox, the landing
  and status page mockups): this skill applies in full.
- **Legacy console** (`web/src/**` on shadcn/ui): follow the frontend rules in
  `CLAUDE.md`. Do not restyle legacy screens piecemeal to the new system; a
  screen moves to the new system whole, when its migration is scheduled.
- **Plugin UI** (a separate document in an iframe): see "Plugin UI" below. It
  gets the system by bundling the package, not by inheriting anything.
- **The package** (`web/packages/op`): change a primitive only together with
  its entry in the handoff doc §6, the gallery on `/op-components` and the
  `CHANGELOG.md` of the package.

## Procedure for a UI task

1. Read `design-system/docs/RULES.md` end to end. It is 120 lines. When it
   disagrees with the two long docs, the long docs win; fix the digest.
2. Classify the screen from its data, not from habit (RULES.md "Page
   structure"): many records of one kind → `Ledger`; one record read top to
   bottom → `Detail` + `Columns`; a configuration → `Settings`; nothing yet,
   not set up or failed → `PageState`.
3. Find the closest reference screen in `design-system/src/sections/` and
   start from its shape. Deployment (`ConsoleV1Deploy.tsx`), Nodes
   (`ConsoleV1Nodes.tsx`), Database (`ConsoleV1Database.tsx`) and Settings
   (`ConsoleV1Settings.tsx`) cover the record, list, tool and configuration
   cases.
4. Build with primitives from `@temps-sdk/op` only. Import the skin once
   (`@import '@temps-sdk/op/op.css'`) and put `operator ink v1` on the
   root you want skinned, including portalled content.
5. Apply the record recipe: title + meta → status verdict → `Lede` with four
   to six facts → `Columns` (main: the thing and its timeline; aside: what is
   left) → sections. A fact appears once. Colour only through `Status`, as
   glyph + word + tone. Icons say what kind, glyphs say what state.
6. Wire every drawn control. A `Kbd` badge needs a handler, a filter must
   filter, a destination is a typed `/${string}` path, never `#`. The ledger
   cursor moves DOM focus.
7. Check both widths, both modes: 1440 and 390, light and dark. Below md,
   ledger rows render `mobile` and it carries the row's primary action.

## Adding a primitive

1. Build it in `web/packages/op/src/*.tsx` and export it from `src/index.ts`.
2. Give it a gallery block: a `<section id="…">` in
   `design-system/src/sections/blocks/*.tsx`, or in `OpComponents.tsx` for a
   primitive that belongs to no rule document. Show every state, not a happy
   path. Add its id and label to the page's TOC (`OpComponents.tsx`, or the
   `*_TOC` export the page spreads).
3. Add the id to `BLOCKS` in `design-system/e2e/visual.spec.ts`, in page order.
   A `toEqual([...BLOCKS])` assertion compares that list against the sections
   the page renders, so the run stays red until both agree.
4. Shoot the baseline: run the gallery tests, then adopt **only** the new
   block's actuals — copy each `-actual.png` Playwright wrote under
   `test-results/` over
   `design-system/e2e/__screenshots__/visual.spec.ts/<name>-<project>.png`.
   Never a blanket `--update-snapshots`: it rewrites ~85 blocks for sub-pixel
   noise and the diff you were meant to read drowns.
5. Write the handoff §6 entry: what it is for, what it refuses to do, its
   states.
6. Add the `CHANGELOG.md` line in `web/packages/op/`.

## Before you ship

Run from `design-system/`:

```bash
bun run lint   # tsc --noEmit + scripts/audit-records.mjs + tokens.mjs check
bun run e2e    # overflow at 390/1440, keyboard, drop focus, reload signatures, axe, visual
```

Both must be clean. Fix dev-console warnings from `Lede` and `Detail`. When a
visual baseline changes on purpose, adopt the actuals as in step 4 above and
say which blocks moved, and why, in the PR.

`audit-records.mjs` audits `src/sections` by default and takes `--dir <path>`
(repeatable) for any other folder of screens:

```bash
node scripts/audit-records.mjs --dir ../examples/example-plugin/web/src
```

**Know what green proves.** Lint-enforced: types (`tsc`), the record recipe
(`audit-records.mjs`, literal-only and heuristic on two of its rules), and
`tokens.json` against `op.css` (`tokens.mjs check`). E2E-enforced: no
horizontal scroll at 390/1440 with a clean console, no new serious/critical axe
violation in light and dark, the keyboard contract, reload signatures, and the
visual baselines.

Honour system — nothing fails if you break these: paper and ink only, no second
hue, colour only through `Status`, no cards and one `.op-raise` per screen, no
hex / `oklch()` / palette literal / `ms` literal in a `.tsx`, the closed
spacing scale, every drawn control wired, view state in the URL beyond what
`state.spec.ts` samples, and the words (`content.md`, `localisation.md`,
`icons.md`). A green lint means the types, the recipe and the tokens hold; it
does not mean the screen follows the design system. Read it yourself.

## Plugin UI

A plugin's UI is a **separate document**: `web/src/pages/plugins/PluginPage.tsx`
mounts it in a same-origin iframe at `/api/x/{plugin}/ui/`, and the plugin
serves its own HTML, JS and CSS. Nothing crosses that boundary — not the
console's stylesheet, not the `operator ink v1` root, not the fonts, not the
Tailwind build that generated the utilities the primitives use. A plugin that
assumes it inherits the skin renders unstyled.

So a plugin sets the system up for itself, like any outside app. It is a plain
Vite + React app (`examples/example-plugin/web/` is the shape), so
`web/packages/op/README.md` is the setup, verbatim:

1. `bun add @temps-sdk/op` plus its peer dependencies, and **pin the version
   the console ships** — two versions of the skin side by side drift in a way
   that reads as "this page looks slightly wrong", not as a bug.
2. `@import '@temps-sdk/op/op.css'` at the top of the entry stylesheet, and
   `@source "../node_modules/@temps-sdk/op/dist"` so the consumer's Tailwind
   scans the package and generates the utilities it renders.
3. `operator ink v1` on the plugin's own root, and on any portalled content.
4. **Theme:** there is no theme channel today. `PluginPage.tsx` syncs the route
   (hash or `postMessage`) and nothing else, so read `prefers-color-scheme` for
   now and toggle `.dark` from it; theme sync from the parent is a follow-up
   (handoff §15).
5. Same conventions as a console screen: `CopyAction` for a copy, `Button busy`
   for an action in flight, `useUrlState` for the view. A plugin's route is
   mirrored into the console's address bar, so a plugin that keeps its facet in
   React state produces a link that does not reopen what the reader was looking
   at.

Handoff §3b says the same thing at length.

## Changing a rule

A rule changes in one commit that edits `brand-guidelines.md`,
`design-system-handoff.md`, `RULES.md` and the reference page together. A
rule stated in only one place is not a rule.

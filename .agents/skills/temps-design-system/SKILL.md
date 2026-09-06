---
name: temps-design-system
description: >
  Build or review console UI so it reads as Temps: the paper-and-ink operator
  design system (`@temps-sdk/op` primitives, the `operator ink v4 v5` skin,
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
| Reference implementation of every screen | `design-system/src/sections/ConsoleV5*.tsx` |
| Browsable guide, component gallery, console mockups | `cd design-system && bun install && bun run dev` → `/guide`, `/op-components`, `/v5` |

## Scope boundary

- **Redesign work** (new screens on `@temps-sdk/op`, the sandbox, the landing
  and status page mockups): this skill applies in full.
- **Legacy console** (`web/src/**` on shadcn/ui): follow the frontend rules in
  `CLAUDE.md`. Do not restyle legacy screens piecemeal to the new system; a
  screen moves to the new system whole, when its migration is scheduled.
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
   start from its shape. Deployment (`ConsoleV5Deploy.tsx`), Nodes
   (`ConsoleV5Nodes.tsx`), Database (`ConsoleV5Database.tsx`) and Settings
   (`ConsoleV5Settings.tsx`) cover the record, list, tool and configuration
   cases.
4. Build with primitives from `@temps-sdk/op` only. Import the skin once
   (`@import '@temps-sdk/op/op.css'`) and put `operator ink v4 v5` on the
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

## Before you ship

Run from `design-system/`:

```bash
bun run lint   # tsc + scripts/audit-records.mjs (record page checklist)
bun run e2e    # overflow at 390/1440, keyboard, drop focus, axe, visual baselines
```

Both must be clean. Fix dev-console warnings from `Lede` and `Detail`. When a
visual baseline changes on purpose, update it with `bun run e2e:update` and
say so in the PR.

## Changing a rule

A rule changes in one commit that edits `brand-guidelines.md`,
`design-system-handoff.md`, `RULES.md` and the reference page together. A
rule stated in only one place is not a rule.

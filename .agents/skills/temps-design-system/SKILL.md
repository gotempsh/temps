---
name: temps-design-system
description: Classify a console screen (list/record/form), build it on @temps-sdk/ds templates and the shared shadcn primitives in @temps-sdk/ui, and follow the console's status vocabulary and status colour rules. Use for any new or changed console UI in web/src, and when building or extending web/packages/ds itself.
---

# Temps console design

Two documents govern console UI, and they cover different ground:

- **`DESIGN.md`** (repo root): the existing, authoritative reference for
  `web/src` as it stands today — full-width pages, compact empty states,
  shared tables/pagination, shadcn controls.
- **`web/packages/ds/docs/RULES.md`**: the same conventions, codified into a
  real package (`@temps-sdk/ds`) with templates, a token pipeline, and lint
  enforcement. Read `RULES.md`, `brand-guidelines.md`, and
  `design-system-handoff.md` in `web/packages/ds/docs/` before building or
  reviewing anything that touches it.

Both describe the *same* visual system (Vercel-inspired Geist theme,
near-black primary, color reserved for state). Neither is the old "operator
ink" prototype (`@temps-sdk/op`, briefly `@temps-sdk/ds`, PR #915) — that had
a different visual language (a glyph vocabulary, an "ink" skin class), zero
consumers, and was deleted. Never reference it, its mockups, or its styling.

## Scope boundary — read this before touching `web/src`

`@temps-sdk/ds` is real and lint-enforced, but **production migration of
`web/src` is partial.** Do not assume an existing screen should move
onto the new templates just because it fits one. The numbered follow-ups in
`design-system-handoff.md` (≈39 hand-rolled page headers, ≥14 stat-tile/chart
call sites, `empty-placeholder`/`empty-state` consolidation, `useGlobalView`)
track remaining work; completed references are noted in the handoff. Migrating one of them is a deliberate task with
its own review, not a drive-by change bundled into unrelated work.

What IS in scope without asking:
- Any **new** screen or panel in `web/src` — build it on the templates.
- Any change to `web/packages/ds` itself (new primitive, new template variant).
- The `design-system/` sandbox app.

## Classification procedure: data shape → template

For any screen (new, or one you're deliberately migrating), ask what it
actually shows:

1. **A collection of same-shaped things** (projects, deployments, logs,
   alerts) → `Ledger`. Columns + rows + optional toolbar/pagination.
2. **One thing, in detail** (a single deployment, project, backup) →
   `Detail`, following the record recipe below.
3. **Input the user submits** (settings, onboarding/setup wizard) →
   `Settings`, built from `Field`/`FormErrors`.
4. **None of the above cleanly fits** (a dashboard, a custom visualization) —
   don't force a template. Compose primitives (`PageHeader`, `Status`,
   `TimeChart`, `Callout`) directly and say so in the PR description.

If the screen has no data yet (new feature, no operator config) — that's not
"skip the template," that's `PageState` variant `not-set-up`. See CLAUDE.md's
feature-discoverability rule: show the surface, state what's missing, give a
concrete example, link to the settings page. Never render nothing.

## The record recipe

Every `Detail` page, no exceptions: **title → verdict → 4-6 facts → main
column → aside**.

- Title: `PageHeader`'s `title`.
- Verdict: one `Status` badge answering "is this OK?" — `PageHeader`'s
  `verdict` slot, directly under the title.
- Facts: 4-6 scannable key/value pairs. More than 6 means some of them
  belong in `main`, not the fact grid.
- Main: the record's actual content (logs, config, timeline).
- Aside: optional secondary content (related resources, metadata).

## The wired-control rule

A control that can't act yet must *look* inert, not merely be `disabled`.
Two concrete patterns in this package:

- `Button`'s `busy`/`busyLabel`: never sets the native `disabled` attribute
  mid-action (it breaks focus and some screen readers stop announcing the
  button right when the user needs to know their click registered). Uses
  `aria-disabled` plus a click guard instead.
- `EchoDialog`'s confirm button: same pattern, gated on the typed phrase
  matching exactly, for irreversible actions (delete, revoke, drop).

Apply this rule to any new control that has a "not ready yet" state — don't
reach for plain `disabled` by default.

## Commands

```
cd web/packages/ds
bun run lint             # typecheck + tokens:check + audit:records
bun run tokens:build     # regenerate src/tokens.css after editing tokens.json
bun run tokens:check     # fails on tokens.json / tokens.css / globals.css drift
bun run audit:records --dir src   # (via lint) raw hex/oklch/px/ms scan

cd design-system
bun run build   # sandbox app; see handoff workspace-install caveat before installing
```

## Adding a primitive

1. Check `design-system-handoff.md`'s primitive catalogue first — most needs
   are an existing primitive's missing prop, not a new component.
2. If it's genuinely new: add it under `web/packages/ds/src/`, export it from
   `src/index.ts`, and reuse existing app code where one already exists
   (relative-path re-export from `web/src`, same pattern `@temps-sdk/ui`
   already uses — see `Kbd`, `CopyAction`, `TimeChart` for examples) rather
   than duplicating it.
3. Add it to the gallery in `design-system/` (`/components`) with a working
   demo.
4. Update the primitive catalogue table in `design-system-handoff.md` and,
   if it changes a rule (not just adds a component), `RULES.md`.
5. Run `bun run lint` from `web/packages/ds` before committing.

## Machine-checked vs honour-system

| Rule | How it's checked |
|---|---|
| Tokens match `globals.css`/`tokens.css` | `tokens:check`, machine |
| No raw hex/oklch/px/ms literal in `web/packages/ds/src` | `audit-records.mjs`, machine |
| TypeScript types (incl. `PageState` `not-set-up`'s required props) | `typecheck`, machine |
| Record recipe order, "not set up" copy actually being concrete | Honour-system — review by eye |
| Color used only for state, never decoration | Honour-system |
| Correct template chosen for a screen's data shape | Honour-system — this skill's classification procedure |
| Production `web/src` migration follow-ups | Partially migrated; remaining sites tracked, not enforced |

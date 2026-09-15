---
name: temps-design-system
description: Follow the authoritative Temps console layout, empty-state, table, and control standards.
---

# Temps console design

Read the repository root `DESIGN.md` completely before building or reviewing
console UI. It is the single source of truth for page structure, compact empty
states, tables, pagination, controls, responsive behavior, and verification.

The separate prototype app is retired. Do not reference its mockups or recreate
its styling. Use the console's existing shared shadcn/ui components in
`web/src/components/ui` and layouts in `web/src/components/layout`.

The retained `web/packages/ds` package is not a mandate to migrate the console.
Do not add it to a screen without an explicit product decision.

For each task, identify the affected screen and states, implement the rules in
`DESIGN.md`, and follow its review checklist. Report remaining legacy gaps
honestly; changing the guide alone does not migrate existing UI.

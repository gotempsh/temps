# Temps design system — mockup library

Standalone, browsable reference for the Temps operator design system: every
foundation, component state and page pattern, rendered with the real
primitives from [`@temps-sdk/op`](../web/packages/op) under the v1 skin
(`operator ink v1`). What you see here is what shipping it in `temps/web`
looks like — not a redrawn approximation.

This is **not wired into the live console**. It's a separate app on purpose:
no backend, no auth, no TanStack Query — just the primitives and composed
patterns, browsable without running `start-temps`.

## Running it

```bash
cd temps/design-system
bun install
bun run dev      # http://localhost:5183
```

`bun run build` and `bun run lint` also work standalone. `lint` is three
checks: `tsc --noEmit`, `scripts/audit-records.mjs` (the eight record rules)
and `node ../web/packages/op/scripts/tokens.mjs check`, which reads
`web/packages/op/tokens.json` against `op.css` and fails with a diff when a
value, a name or the order of the two disagrees (`bun run tokens:check` runs
it alone).

## Routes

| Route | Covers |
|---|---|
| `/guide` | The reading entry point: `docs/*.md` rendered as one page, with live blocks in place of prose where a rule is better shown than described |
| `/brand` | The decided brand: positioning, plan ladder as design input, paper + ink, type role, signature moves, voice |
| `/foundations` | Type hierarchy by weight, paper/ink tokens (light + dark), colour = status (five states), density and rhythm, radius 0.25rem, motion, responsive rules |
| `/components` | The primitives under ink: button, input, picker vs select, checkbox/switch, tabs vs segmented, rows, palette, popover/menu, dialog, toast, skeleton, breadcrumb + page title, plus the banned list with replacements |
| `/op-components` | Every operator component in `@temps-sdk/op`, every state, with props |
| `/patterns` | The three page templates live (Ledger, Detail, Settings), PageState, promote/roll back, per-environment variables with bulk association, time and retention, keyboard model, responsive folds |
| `/kitchen-sink` | Stress test: the whole v1 console at 390/768/1024/1280, pathological data, every state of every component, dark, dense, charts and forms at the limit, and the banned gallery (the old look, greyed, each item naming its replacement) |
| `/v1` | Operator console v1 (three templates, PageState, sampled status; the plan is a fixture, Cloud Starter) |
| `/v1?fresh=1` | The same console as a fresh install: nothing configured, nothing recorded. Combines with `?p=…`, e.g. `/v1?p=email&fresh=1` |
| `/v1-landing` | Landing page in the same system, with pricing, limits table, mobile menu, frozen accent |
| `/status-page` | The public status page for a project, inside the sandbox chrome |
| `/agent` | Agentic conversation on v1: AI Elements vocabulary (tools in six states, approvals, subagent, plan, tasks, question, queue, checkpoint, prompt bar with model/thinking/mode/workspace/context) |

Three routes render the same surfaces chrome-free, as a real user would see
them: `/console`, `/landing` and `/status?project=…`. The ⤢ button in the
sandbox header toggles between each pair.

`/` redirects to `/guide`. Every route above renders inside one shell,
`src/components/Layout.tsx`: top bar (mark, filter, light/dark), a left rail
listing the guide's sections and then the reference pages, the page itself
starting at the column's left edge, and one "on this page" rail on the right
that the guide and `src/components/op-doc.tsx` both feed. Toggle light/dark in
the header — every surface should look correct in both.

## The package

The operator primitives are **not** copied into this app. They live in
[`web/packages/op`](../web/packages/op) as the `@temps-sdk/op` workspace
package (components plus `op.css`, the whole skin), and the sandbox consumes
them by source through a Vite alias in `vite.config.ts` — so there is no
build step between editing a primitive and seeing it here, and no drift
between what this library shows and what `temps/web` imports.

`src/components/ui/*` and `src/globals.css` are still copies of
`temps/web/src/components/ui` and `temps/web/src/globals.css`. Those **will
drift** if the source files change and this folder isn't updated: re-copy the
affected file(s) and re-verify the affected route before treating this as
current.

## Tests

Playwright, Chromium only, in `e2e/`:

```bash
bun run e2e          # the whole suite
bun run e2e:ui       # pick and step through tests interactively
bun run e2e:update   # rewrite the visual baselines
```

It reuses a dev server already answering on the port it is pointed at and
starts one only if nothing does, so you can leave your tab open. The port is
5183 by default; set `DS_PORT` to run against a different one (useful when a
second checkout already owns 5183):

```bash
bun run dev --port 5184 --strictPort
DS_PORT=5184 bun run e2e
```

The suites are accessibility (`a11y`), keyboard and focus return
(`keyboard`), overflow at 390px (`overflow`), the drop/`Drop` primitive
(`drop`), and visual baselines (`visual`). Baselines live in
`e2e/__screenshots__/` and are committed; only regenerate them from a quiet
dev server, because a baseline captured mid-refactor bakes the broken state
in.

## Docs

`docs/` is the written half of the system, and `/guide` renders it:

- `design-system-handoff.md` — the handoff: how to run it, what exists, every
  decision and its reason. Start here if you are picking this up fresh.
- `brand-guidelines.md` — direction, type scale, colour, signature moves.
- `RULES.md` — imperative digest for coding agents, rendered at
  `/guide#tooling`.
- `ux-audit-2026-09-06.md` — the audit the guide's "open questions" section
  is cut from.
- `forms.md` — field anatomy, validation timing, saving, long submits,
  destructive submits, secrets. Live at `/guide#forms`.
- `notifications.md` — which surface says it: status line, Callout, toast,
  bell, EchoDialog. Live at `/guide#notifications`.
- `content.md` — the words: capitalisation, one term per concept, the shape of
  an error, buttons, numbers and time. Live at `/guide#content`.
- `localisation.md` — text expansion, no sentences built from fragments,
  logical properties, RTL readiness. Rendered at `/guide#locale`.
- `data-viz.md` — which chart answers which question, series told apart by
  pattern instead of hue, axes, annotations, the footer contract. Live at
  `/guide#dataviz`.
- `motion.md` — the three duration tokens, what may move, what never moves,
  reduced motion, the two exceptions. Live at `/guide#motion`.
- `icons.md` — lucide only, two sizes, and the concept → icon vocabulary that
  stops two screens using two icons for one thing. Live at `/guide#icons`.
- `console-inventory.md`, `design-system-answers.md`,
  `operator-console-brief.md` — background: what the console contains, the
  twelve questions a design system must answer, and the original brief
  (historical, do not edit).

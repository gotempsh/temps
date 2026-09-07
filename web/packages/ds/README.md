# @temps-sdk/op

The Temps **operator design system**: the primitives every console screen is
built from, plus the skin they render in, as one self-contained package.

This is not a shadcn re-export. The `src/ui/*` files here are a *private,
minimal* copy of the shadcn-style primitives the op components need — they are
deliberately **not** re-exported from `web/src`, so the package can be dropped
into any app (or reskinned) without dragging the console's whole UI layer
along.

## Install

Inside this repo it is a bun workspace of `web`, so the console and the
sandbox get it for nothing. Outside it:

```bash
bun add @temps-sdk/op
```

The peer dependencies are the libraries that must be a single copy in the app:
`react`, `react-dom`, `react-router`, `lucide-react`, `class-variance-authority`,
`clsx`, `tailwind-merge`, `sonner` and the five `@radix-ui/*` packages the
primitives render (`react-alert-dialog`, `react-dialog`, `react-popover`,
`react-slot`, `react-tooltip`). `cmdk`, `recharts` and `react-simple-maps` are
ordinary dependencies — nothing breaks if the app also has its own.

The published build is bundler-targeted ESM: `dist/*.js` with extensionless
relative specifiers, plus `.d.ts` and source maps. Vite, rsbuild and webpack
resolve it as-is; Node's own ESM loader would not, and this package is not
meant to be imported outside a bundler.

## Setup, in four lines

**1. The stylesheet, first thing in the Tailwind v4 entry CSS** (`@import`
rules must precede every rule):

```css
@import 'tailwindcss';
@import '@temps-sdk/op/op.css';
@source "../node_modules/@temps-sdk/op/dist";
```

The `@source` line is not optional and it is the step that gets forgotten.
The primitives are written in Tailwind utility classes, and Tailwind only
generates a class it has seen in a file it scanned; without it the components
mount with the tokens applied and no layout. `op.css` carries `@source "./"`
for consumers that resolve the package to *source* (this monorepo, and the
design-system sandbox through its Vite alias), where the TSX is beside the
stylesheet — an installed copy publishes `dist/`, not `src/`, so the app has
to point at `dist` itself.

**2. The skin class on the root you want skinned:**

```tsx
<div className="operator ink v1">…</div>
```

All three words. The CSS chains them (`.operator.ink.v1`): `operator` is the
token block, `ink` the paper-and-ink skin, `v1` the published system.
`operator hardline` is the separate landing skin. The tokens are scoped to
`.operator`, never `:root`, so the package cannot fight an app's own theme.

**3. Dark mode** is `.dark` on (or above) that root — the same convention
`next-themes` and the console use. Nothing else switches.

**4. Portals.** Dialogs, toasts, tooltips and command palettes render outside
the root, so their content needs the class too:

```tsx
<DialogContent className="operator ink v1">…</DialogContent>
```

Then import primitives:

```tsx
import { Ledger, Status, Metric, TimeChart, useUrlState } from '@temps-sdk/op'
```

### Fonts

The skin sets `--font-sans` and `--font-mono` to **Geist Mono**, with
`ui-monospace` / `SF Mono` / Menlo behind it, and `.op-prose` falls back to
Geist Sans for wrapping text. The package ships no font files and loads
nothing: an app that wants the real faces loads Geist and Geist Mono itself
(self-hosted or from a CDN). Without them the fallbacks render and the layout
holds — the metrics are close enough that nothing reflows badly.

### Version alignment

Match the version the console ships. A plugin or a downstream app that renders
beside the console and pins a different minor will drift visibly — a token
value, a glyph, a row height — and the drift shows up as "this page looks
slightly wrong", which is the hardest kind of bug to report. The console's
`web/package.json` (and, in this repo, `web/packages/op/package.json`) is the
number to match; `CHANGELOG.md` in this package is the record of what changed
between two of them.

### Bundler note

The package lives under `web/node_modules`' scope. Any consumer outside `web`
(the design-system sandbox, for one) must dedupe React or it will load a second
copy and throw *"Invalid hook call"*:

```ts
resolve: { dedupe: ['react', 'react-dom', 'react-router'] }
```

The sandbox goes further and aliases the package to its source
(`design-system/vite.config.ts`), so an edit to a primitive hot-reloads
without a build step. The `"source"` export condition says the same thing to
bundlers that honour it.

### Building

```bash
bun run build   # tsc -p tsconfig.build.json → dist/ (ESM + .d.ts + maps)
```

`dist/` is generated and git-ignored. `op.css` and `tokens.json` are published
from their source paths, so `@temps-sdk/op/op.css` and
`@temps-sdk/op/tokens.json` resolve the same whether the consumer is on the
tarball or on the workspace.

## Read these three before adding a primitive

Do not copy them here — they are the source of truth and they move:

1. `design-system/docs/brand-guidelines.md` §6 — the op layer: what the skin
   is allowed to do, colour-means-status-only, the white/black rule.
2. `design-system/docs/design-system-handoff.md` §6 — the primitive catalogue
   and when to reach for each one.
3. `design-system/docs/design-system-handoff.md` §7 — the page templates
   (`Ledger`, `Detail`, `Settings`) and how a screen is assembled from them.

## The rule

**Every primitive follows the record recipe.** A screen is a record: identity
line, then status, then the facts, then the actions — never a grid of cards.
A new primitive earns its place only by making some record read faster; if it
decorates, it does not belong here. `design-system/scripts/audit-records.mjs`
enforces the mechanical half of this, and `bun run lint` in the sandbox runs it.

## Layout

```
src/
  index.ts          the public surface — everything below is exported here
  *.tsx             the op primitives
  url-state.ts      the URL *is* the view state: useUrlState & friends
  ui/               private shadcn-style primitives the op layer needs
  lib/cn.ts         the class merger
  lib/clipboard.ts  clipboard with a non-secure-origin fallback
  assets/geo/       countries-110m topojson, for <GeoMap>
  op.css            tokens + every .op-* rule
```

## The URL is the state

`useUrlState`, `useUrlNumber`, `useUrlPatch`, `useUrlWindow`, `useUrlSort` and
`useUrlText` put the view — facet, filter, sort, page, range, inspected row —
in the query string, so a reload, a pasted link and a second tab rebuild the
same screen. They sit on `react-router`'s `useSearchParams` and must be called
under a router. The import is bare **`react-router`**, not `react-router-dom`:
that is the package the console and the sandbox are both on (v8, where
`react-router-dom` is only a compatibility shim).

`forNewView(params)` drops the view state when the reader navigates to another
record, keeping only the routing keys — `p`, `fresh`, `fail` by default
(`KEPT_ON_NAVIGATION`). An app with other routing conventions passes its own
list: `forNewView(params, ['project', 'env'])`.

The rules these hooks exist to enforce are in
`design-system/docs/requirements.md`; the catalogue entry is
`design-system/docs/design-system-handoff.md` §6 "useUrlState".

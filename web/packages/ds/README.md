# @temps-sdk/ds

The retained Temps operator UI package, for existing consumers. It is not the
console's current design standard. Console contributors must follow
[DESIGN.md](../../../DESIGN.md) and use the existing shared shadcn/ui components.

The prototype app has been removed. References to its files in source comments
and historical changelog entries describe the retired implementation; retrieve
them from the [archived prototype](https://github.com/gotempsh/temps/tree/5169ea3513f99462b872a3a891cdfe47a874923f/design-system).
They are not instructions to run a current app or migrate the console.

This is not a shadcn re-export. The `src/ui/*` files here are a *private,
minimal* copy of the shadcn-style primitives the op components need — they are
deliberately **not** re-exported from `web/src`, so the package can be dropped
into any app (or reskinned) without dragging the console's whole UI layer
along.

## Install

Inside this repo it is a Bun workspace of `web`. Workspace membership does not
mean the console imports it. Existing external consumers can install it with:

```bash
bun add @temps-sdk/ds
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
@import '@temps-sdk/ds/op.css';
@source "../node_modules/@temps-sdk/ds/dist";
```

The `@source` line is not optional and it is the step that gets forgotten.
The primitives are written in Tailwind utility classes, and Tailwind only
generates a class it has seen in a file it scanned; without it the components
mount with the tokens applied and no layout. `op.css` carries `@source "./"`
for consumers that resolve the package to *source*, where the TSX is beside the
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
import { Ledger, Status, Metric, TimeChart, useUrlState } from '@temps-sdk/ds'
```

### Fonts

The skin sets `--font-sans` and `--font-mono` to **Geist Mono**, with
`ui-monospace` / `SF Mono` / Menlo behind it, and `.op-prose` falls back to
Geist Sans for wrapping text. The package ships no font files and loads
nothing: an app that wants the real faces loads Geist and Geist Mono itself
(self-hosted or from a CDN). Without them the fallbacks render and the layout
holds — the metrics are close enough that nothing reflows badly.

### Version alignment

Existing consumers should pin a compatible package version and review
`CHANGELOG.md` when upgrading. The console does not currently consume this
package, so its package manifest does not prescribe a version for plugins.

### Bundler note

Workspace consumers must deduplicate React, React DOM, and React Router to
avoid loading multiple copies and causing invalid hook calls. For Vite:

```ts
resolve: { dedupe: ['react', 'react-dom', 'react-router'] }
```

The `"source"` export condition is available to bundlers that support it.

### Building

```bash
bun run build   # tsc -p tsconfig.build.json → dist/ (ESM + .d.ts + maps)
```

`dist/` is generated and git-ignored. `op.css` and `tokens.json` are published
from their source paths, so `@temps-sdk/ds/op.css` and
`@temps-sdk/ds/tokens.json` resolve the same whether the consumer is on the
tarball or on the workspace.

## Maintenance scope

The package remains available for existing consumers. Its former sandbox,
gallery, and audit commands are no longer present. Do not claim those checks
ran. Check the package build and the actual consuming application when changing
a primitive. New console design work follows root `DESIGN.md`, not the
archived prototype rules.

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
that is the router package used by this implementation.

`forNewView(params)` drops the view state when the reader navigates to another
record, keeping only the routing keys — `p`, `fresh`, `fail` by default
(`KEPT_ON_NAVIGATION`). An app with other routing conventions passes its own
list: `forNewView(params, ['project', 'env'])`.

See the archived prototype linked above for the original hook design notes.

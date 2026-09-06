# @temps-sdk/op

The Temps **operator design system**: the primitives every console screen is
built from, plus the skin they render in, as one self-contained package.

This is not a shadcn re-export. The `src/ui/*` files here are a *private,
minimal* copy of the shadcn-style primitives the op components need — they are
deliberately **not** re-exported from `web/src`, so the package can be dropped
into any app (or reskinned) without dragging the console's whole UI layer
along.

## Install / import

It is a workspace package inside `temps/web`, so consumers just import it:

```tsx
import { Ledger, Status, Metric, TimeChart } from '@temps-sdk/op'
```

And **once**, in the app's Tailwind entry stylesheet (this is the whole skin —
tokens plus every `.op-*` rule):

```css
@import 'tailwindcss';
@import '@temps-sdk/op/op.css';
```

`op.css` starts with `@source "./"` (relative to the file), so Tailwind scans
this package's own TSX and generates the utilities it uses. Consumers do not
need to add the package to their own `@source` list.

The skin is applied by putting `operator ink` (or `operator`, or
`operator hardline`) on a root element — the tokens are scoped to `.operator`,
not to `:root`, so the package never fights an app's existing theme.

### Bundler note

The package lives under `web/node_modules`' scope. Any consumer outside `web`
(the design-system sandbox, for one) must dedupe React or it will load a second
copy and throw *"Invalid hook call"*:

```ts
resolve: { dedupe: ['react', 'react-dom', 'react-router'] }
```

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
  ui/               private shadcn-style primitives the op layer needs
  lib/cn.ts         the class merger
  lib/clipboard.ts  clipboard with a non-secure-origin fallback
  assets/geo/       countries-110m topojson, for <GeoMap>
  op.css            tokens + every .op-* rule
```

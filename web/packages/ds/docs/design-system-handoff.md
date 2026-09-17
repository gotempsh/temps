# @temps-sdk/ds — handoff

## What this is

A codification effort, not a redesign: `@temps-sdk/ds` formalizes the page,
record, list and settings patterns already implied by `web/src` — Vercel-
inspired Geist theme, near-black primary, color reserved for state — into one
reusable package. It replaces the retired "operator ink" package (née
`@temps-sdk/op`, briefly `@temps-sdk/ds`, PR #915), which invented a new
visual skin with zero consumers and was deleted at the start of this branch.
Nothing in this package resurrects its glyph vocabulary, its "ink" skin
class, or its sandbox.

Full brief and the six interview decisions: `web/docs/design/decisions.md`.
Imperative rule digest: `docs/RULES.md`. Why these choices: `docs/brand-guidelines.md`.

## Setup

```
web/package.json          -> "@temps-sdk/ds": "workspace:*"
web/packages/ds/package.json -> depends on @temps-sdk/ui, peers react/react-dom/react-router
```

No build step — mirrors `@temps-sdk/ui`/`@temps-sdk/console-kit`: `exports`
point straight at `src/index.ts`, consumed as TypeScript source through the
bun workspace and whatever bundler the consumer already uses (rsbuild for
`web/src`, Vite for the `design-system/` sandbox).

```
bun install                        # from web/, resolves the workspace package
cd web/packages/ds && bun run lint # typecheck + tokens:check + audit:records
cd design-system && bun install && bun run build   # sandbox typechecks/builds
```

## Tokens

`tokens.json` (W3C DTCG: `base` primitives, `semantic.light`/`semantic.dark`
as separate authored layers) mirrors `web/src/globals.css` exactly —
`scripts/tokens.mjs check` fails the build if they drift. `src/tokens.css` is
generated from `tokens.json`, scoped to `.tds` (never `:root`).

| Group | Examples | Source |
|---|---|---|
| Color (base) | `gray-0..900`, `blue`, `green-success`, `green-chart`, `amber`, `red`, `purple` | `tokens.json` `base.color` |
| Color (semantic) | `background`, `primary`, `success`, `chart-1..5`, `sidebar*` | `tokens.json` `semantic.light`/`.dark` |
| Radius | `sm` 0.25rem, `md` 0.375rem, `lg` 0.5rem (default), `xl` 0.75rem | `base.radius` |
| Type | Geist / Geist Mono | `base.font` |
| Shadow | `2xs`..`2xl`, distinct light/dark opacity+blur | `semantic.*.shadow` |

## Primitive catalogue

| Primitive | Key props | When | Enforced |
|---|---|---|---|
| `PageHeader`/`PageContainer` | `title`, `description`, `verdict`, `actions` | Every page | Honour-system |
| `Status`/`StatusDot` | `tone` (5 values), `variant` | Any state anywhere | Type-level (tone union) |
| `PageState` | variant `empty`\|`not-set-up`\|`failed`; `not-set-up` requires `requirement`, `example`, `settingsHref` | No data / unconfigured / error | Type-level for `not-set-up` |
| `Button` | `busy`, `busyLabel` | Any async action button | Honour-system |
| `CopyAction` | `value`, `label` | Copyable values | Honour-system |
| `Field`/`FormErrors` | `label`, `error`, `description` | Every form control | Honour-system |
| `Callout` | `tone` (info/success/warning/error) | In-page notices | Honour-system |
| `EchoDialog` | `phrase`, `confirmLabel`, `onConfirm` | Irreversible destructive actions | Honour-system |
| `Picker` | `items`, `value`, `onValueChange` | Inline searchable select | Honour-system |
| `TimeChart` | wraps `ThresholdLineChart` props | Any time series | Honour-system |
| `useUrlState` | `state`, `patch`, `clear` | Any filter/tab/page state | Honour-system |
| `Kbd` | `keys` | Keyboard shortcut hints | Honour-system |
| `fmt.ts` | `fmtNumber`, `fmtBytes`, `fmtDuration`, `fmtRelativeTime`, `fmtDate(Time)` | Any formatted number/date | Honour-system |

## Page templates + record recipe

- **`Ledger`** (list): header, optional toolbar, table, optional pagination.
  Reference screen: `design-system/` "Deployments" list.
- **`Detail`** (record): **title → verdict → 4-6 facts → main column → aside**.
  Reference screen: `design-system/` deployment detail.
- **`Settings`** (form): `Field`s, `FormErrors` above a sticky save bar that
  stays mounted regardless of `dirty`. Reference screen: `design-system/`
  project settings form.

## Responsive & keyboard

`PageHeader` actions wrap under the title below `sm`. `Detail`'s aside stacks
below `main` below `lg`. `Ledger`'s table is the one thing allowed to scroll
horizontally. `Picker` is fully keyboard-filterable from focus; shortcuts get
a visible `Kbd` badge next to their control — never a keyboard-only entry
point (repo-wide discoverability rule, `CLAUDE.md`).

## Tests / enforcement

Lint only, by explicit user decision for this phase (no Playwright visual
baselines, no axe — that was the retired package's approach and is more than
this phase needs):

| Check | Machine-checked | Honour-system |
|---|---|---|
| Tokens match `globals.css`/`tokens.css` | Yes (`tokens:check`) | — |
| No raw hex/oklch/px/ms in `src/` | Yes (`audit-records.mjs`) | Anything outside `--dir` scope (production `web/src` isn't scanned yet) |
| TypeScript types | Yes (`typecheck`) | — |
| Record recipe order, "not set up" copy quality, color-as-state discipline | — | Yes — see `RULES.md` |
| Sandbox screens actually use the templates | — | Yes (reviewed by hand this pass) |

## Follow-ups (numbered, honest — none of these are done)

1. Migrate the ~39 files that hand-roll `className="flex items-center
   justify-between"` instead of `PageHeader` (grep
   `flex items-center justify-between` under `web/src/pages` and
   `web/src/components`). Not attempted this pass beyond the one promoted
   file (`PageContainer.tsx` itself).
2. Migrate the ≥14 hand-rolled stat-tile/chart-panel call sites
   (`MetricTile`, `ProjectOverview`, `ProjectSpeedInsights`,
   `ErrorTimeSeriesChart`, `ServerMonitoring`, `ApiTraffic`, `PageDetail`,
   `EventDetail`, `AnalyticsTrafficChart`, `MetricsExplorer`, `ProxyMetrics`,
   `UserDetail`, `OtelPipelineStatusPage`) onto `TimeChart`.
3. Migrate `web/src/components/ui/empty-placeholder.tsx` and
   `empty-state.tsx` call sites onto `PageState`, then delete both files.
4. Point `web/src/hooks/useGlobalView.ts` at `useUrlState` internally instead
   of hand-rolling its own `URLSearchParams` patching (behavior-preserving
   refactor, not attempted this pass — `useGlobalView` has observability-
   specific normalization logic worth reviewing carefully first).
5. Decompose the `shadow` tokens in `tokens.json` into proper DTCG
   `boxShadow` objects (color/offsetX/offsetY/blur/spread) instead of raw CSS
   strings, if a non-CSS export target is ever needed.
6. Extend `audit-records.mjs` to run against `--dir web/src` once production
   migration starts, and decide on an allowlist for legitimate arbitrary
   Tailwind values already in the app (e.g. chart pixel heights).
7. Cross-repo consumption (vibetemps, temps-fleet) is explicitly out of
   scope for this phase (decision #5) — no packaging/versioning work done
   toward that.
8. `DESIGN.md` (repo root) and `.agents/skills/temps-design-system/SKILL.md`
   were updated to stop calling `@temps-sdk/ds` retired, but still describe
   the console-wide conventions this package doesn't yet enforce outside its
   own sandbox — reconcile the two documents once migration (follow-ups 1-3)
   is underway.
9. Migrate the ~20 remaining `web/src/pages` detail screens that still stack
   raw `<Card>` blocks with no template, no record recipe, and no
   `useUrlState` — the same disease `EmailDetail.tsx` had before this pass.
   By `<Card>` count (highest first): `EmailDomainDetail.tsx` (30),
   `ServiceDetail.tsx` (28), `SandboxDetail.tsx` (28),
   `MajorUpgradeDetail.tsx` (26), `AgentSandboxProviderDetail.tsx` (24),
   `RequestLogDetail.tsx` (22), `ScheduleDetail.tsx` (20),
   `BackupDetail.tsx` (19), `DnsProviderDetail.tsx` (17),
   `IpGeolocationDetail.tsx`/`SessionReplayDetail.tsx`/`ApiKeyDetail.tsx`
   (15-16), plus `security/ScanDetail.tsx`, `S3SourceDetail.tsx`,
   `GitProviderDetail.tsx`, `EmailProviderDetail.tsx`,
   `CrossProjectTraceDetail.tsx` (14 each). `EmailDetail.tsx` is the first of
   these migrated and is the reference example for the rest: `Detail`
   template with a single verdict `Status` derived from the record's own
   status field (not a duplicated badge), 4-6 facts with each value owned by
   exactly one slot, `useUrlState` for every tab/filter/page instead of local
   `useState`, `PageState` (`failed`, with a retry action) for the error
   path, a `Detail`-shaped skeleton for the loading path instead of ad hoc
   `Card`+`Skeleton` stacking, and `CopyAction` always as a sibling of the
   value it copies, never as its wrapper.

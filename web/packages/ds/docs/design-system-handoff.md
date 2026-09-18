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
cd design-system && bun run build   # uses existing sandbox dependencies
```

### Workspace install caveat

The sandbox currently has its own workspace root. Running `bun install`
there can create `web/packages/ds/node_modules` links that shadow the
console's hoisted `react-router`, causing a runtime "must be used within a
Router" error despite a passing typecheck. Use existing dependencies when
building the sandbox. If a fresh sandbox install is needed, install there
first, then remove the generated `web/packages/ds/node_modules` directory
and run `bun install` from `web/` to restore console dependency resolution.
Restart the dev servers afterward. This is a documented workaround; merging
both workspace roots remains open work.

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
| `DateTimeRange` / `TimeRangeFilter` | controlled value + `onChange`, `maxRangeDays?` | Compact 1h / 6h / 24h / 7d shortcuts and validated custom local date/time; `TimeRangeFilter` accepts URL strings | Existing date-range unit tests |
| `ResponsivePagination` | `page`, `pageSize`, `total`, `totalPages`, `onPageChange` | Tables with counts, mobile controls, and optional page-size changes; promoted console implementation | Existing console behavior |
| `TimeChart` | wraps `ThresholdLineChart` props | Any time series | Honour-system |
| `useUrlState` | `state`, `patch`, `clear` | Any filter/tab/page state | Honour-system |
| `Kbd` | `keys` | Keyboard shortcut hints | Honour-system |
| `LogLine` | `content`, `isHighlighted`, `searchTerm` | One row of a monospace log stream | Honour-system |
| `ResourceStat` | `icon`, `value`, `limit?` | Inline CPU/memory/disk usage display | Honour-system |
| `fmt.ts` | `fmtNumber`, `fmtBytes`, `fmtDuration`, `fmtRelativeTime`, `fmtDate(Time)` | Any formatted number/date | Honour-system |
| `notify` | `notify.ok(message, description?)`, `notify.fail(message, description?)` | Background events the user isn't watching (RULES.md § Notifications) | Honour-system |
| `Article` | `children` | Long-form content read top to bottom (release notes, postmortems, docs) — not for record/scan pages, that's `Detail` | Honour-system |
| `GitProviderMark` | `provider`, `className?`, `label?` | Existing GitHub/GitLab/Bitbucket/Gitea marks in current text color; branch fallback for unknown providers | Label and fallback unit tests |
| `ProjectAvatar` | `name` | Deterministic project identity where there's no deployment media (pickers, ledger rows, headers) — never a guaranteed-404 favicon fetch | Honour-system |
| `DataTable` | `columns`, `rows`, `rowKey`, `onRowClick?`, `isLoading?`, `aria-label?`, `pagination?` | Any table — embedded (settings sub-panel, `Detail`'s `main`) or as `Ledger`'s body | Honour-system |
| `CompactRow` | `timestamp`, `icon`, `primary`, `secondary?`, `meta?` | One row of a dense event/log/activity list (promoted from Observe's `ObserveRowShell`) | Honour-system |
| `Wizard` | `title`, `description`, `currentStep`, `steps`, `footer?`, `celebrate?` | Any multi-step flow (setup wizard, onboarding, "connect a resource") | Honour-system |

## Page templates + record recipe

- **`Ledger`** (list): header, optional toolbar, table, optional pagination.
  Reference screen: `design-system/` "Deployments" list.
- **`Detail`** (record): **title → verdict → 4-6 facts → main column → aside**.
  Reference screen: `design-system/` deployment detail.
- **`Settings`** (form): `Field`s, `FormErrors` above a sticky save bar that
  stays mounted regardless of `dirty`. Reference screen: `design-system/`
  project settings form.
- **`CardGrid`** (list, card layout): same header shape as `Ledger`
  (title/description/actions/toolbar), a responsive grid body instead of a
  table — one `renderCard(item)` per record, loading skeleton cards, empty
  state, optional pagination. For record collections better shown as cards
  than rows (e.g. `Projects.tsx`'s project grid). Does not include or
  reimplement any specific card component — bring your own (`ProjectCard`,
  etc.) as `renderCard`. Reference screen: `design-system/` "CardGrid —
  Projects".
- **`Wizard`** (multi-step flow): shared page header, labeled progress, and
  an optional bordered step surface with a persistent `footer` for actions.
  Existing consumers without `footer` keep their own content surfaces;
  `celebrate` remains opt-in. Not one of the three original
  templates (`Ledger`/`Detail`/`Settings`) — a fourth shape for input
  collected across steps rather than a single form. Promoted from
  `SetupWizardShell.tsx`. Reference screen: `design-system/` "Wizard —
  Connect a repository" — provider choices, labeled repository input, Back,
  validation, URL-restored selections, and honest sample-only completion.

## Responsive & keyboard

`PageHeader` actions wrap under the title below `sm`. `Detail`'s aside stacks
below `main` below `lg`. `Ledger`'s table is the one thing allowed to scroll
horizontally. `Picker` is fully keyboard-filterable from focus; shortcuts get
a visible `Kbd` badge next to their control — never a keyboard-only entry
point (repo-wide discoverability rule, `CLAUDE.md`).

## Collection-state reference

Sandbox `/table-states` demonstrates an execution overview: four summary
metrics, a `TimeChart`, and paginated history share one filtered collection.
A compact toolbar combines search, result selection, and the existing
`TimeRangeFilter` with 1h / 6h / 24h / 7d and custom date-time windows. All
filters and pagination live in the URL. Example-state controls are collapsed
below the content. Switch between loaded, initial
loading, successful empty, failed, and refresh-failed states. Retry recovers
the sample request without clearing filters; a selection with no matches
has a reset action. Fixture dates are relative to when the page opens, so
presets remain useful; custom ranges preserve absolute timestamps. All records are invented and no API is called.

`DataTable` owns the header, rows, skeleton, border and horizontal scrolling.
The caller owns request errors, empty/no-match copy, filters, and retries.
Give the table an accessible name with `aria-label`. Prefer a real link in
the identity column over row-click-only navigation. Loading cells retain
column visibility/alignment classes. Pagination boundary controls guard
keyboard activation as well as pointer interaction.

Use `RULES.md` → Collection states when migrating another embedded table.
`CronJobDetail.tsx` is the production reference for independent requests and
retaining cached data after refresh failures. The sandbox controls simulate
states; production retry actions must call the relevant query's `refetch`.

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

## Follow-ups (numbered; partial progress noted per item)

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
9. Continue the remaining detail-screen migrations. Seventeen production
   pages now use `Detail`: `EmailDetail`, `EmailDomainDetail`, `ServiceDetail`,
   `SandboxDetail`, `MajorUpgradeDetail`, `RequestLogDetail`, `ScheduleDetail`,
   `BackupDetail`, `DnsProviderDetail`, `IpGeolocationDetail`,
   `SessionReplayDetail`, `ApiKeyDetail`, `security/ScanDetail`,
   `S3SourceDetail`, `GitProviderDetail`, `EmailProviderDetail`, and
   `CrossProjectTraceDetail`. `agent-sandbox/AgentSandboxProviderDetail.tsx`
   remains from the original high-card-count candidate list; assess its
   provider-editing behavior before migrating it.
   `EmailDetail.tsx` is the canonical reference: `Detail`
   template with a single verdict `Status` derived from the record's own
   status field (not a duplicated badge), 4-6 facts with each value owned by
   exactly one slot, `useUrlState` for every tab/filter/page instead of local
   `useState`, `PageState` (`failed`, with a retry action) for the error
   path, a `Detail`-shaped skeleton for the loading path instead of ad hoc
   `Card`+`Skeleton` stacking, and `CopyAction` always as a sibling of the
   value it copies, never as its wrapper.
10. Migrate the 181 files under `web/src` that call `sonner`'s
    `toast.success`/`toast.error` directly (grep
    `toast\.\(success\|error\)(` under `web/src`) onto `notify.ok`/
    `notify.fail`. Not attempted this pass beyond adding the primitive and
    its gallery demo — 181 call sites is a deliberate, separately-reviewed
    migration, not a drive-by change.
11. **`log-viewer.tsx` (1986 lines) and `history-log-viewer.tsx` (1378
    lines)** (`web/src/components/runtime-logs/`) are complex, stateful,
    real-time log-streaming engines — live tail during deployments,
    virtualized scrolling, WebSocket/SSE data flow. They were explicitly
    excluded from this pass by the user and are NOT touched, refactored, or
    "consolidated" here. This is flagged as its own separate, larger,
    higher-risk follow-up requiring dedicated review — not something to fold
    into a routine primitive-promotion pass. Note: their inline
    ANSI-HTML-based `<mark>` highlighting (rendered via
    `dangerouslySetInnerHTML`) is a genuinely different rendering path from
    the promoted `LogLine` primitive's plain-text children, so pointing them
    at `LogLine` isn't a trivial swap even once someone picks this up.
12. Migrate the remaining ~10 CPU/memory/disk stat display call sites onto
    `ResourceStat` (`ContainerList.tsx` is migrated as the first/reference
    example — see its inline CPU/memory row): `ContainerHeaderBar.tsx`,
    `storage/MonitoringCard.tsx`, `storage/ServiceResourcesPanel.tsx`
    (its `Meter` also draws a progress bar with raw `bg-red-500`/
    `bg-amber-500` colors — worth folding into `Status`'s tone vocabulary
    when this is picked up, not just swapping the icon+value row),
    `project/ProjectStorage.tsx`, `project/ProjectOverview.tsx`,
    `monitoring/EnvironmentMetricsCard.tsx`, `ServerMonitoring.tsx`,
    `pages/ServiceMonitoring.tsx`, `pages/settings/NodesPage.tsx` (its local
    `MetricCard` also has a raw-color progress bar, same note as
    `ServiceResourcesPanel`), `pages/Storage.tsx`. Not attempted this pass
    beyond the one migrated site and the primitive itself.
13. Migrate the remaining embedded-table call sites onto `DataTable`
    (`ApiKeyTable.tsx` is migrated as the first/reference example). A grep
    for `TableHeader` outside `web/src/components/ui/table.tsx` found 56 at
    the time of this pass (down from the ~65 counted at audit time — some
    may already have moved under concurrent work in this worktree), roughly:
    - **Settings sub-panels** (embedded, not full pages — highest-value
      first targets, same shape as `ApiKeyTable.tsx`):
      `project/settings/DeploymentTokensSettings.tsx`,
      `project/settings/ProjectAccessSettings.tsx`,
      `project/settings/webhooks/WebhookDetail.tsx`,
      `agents/ProjectSecrets.tsx`, `project/flags/ProjectFeatureFlags.tsx`,
      `monitoring/NodeAlertRules.tsx`, `storage/MonitoringCard.tsx`.
    - **Embedded tables inside detail/analytics panels** (a `Detail`'s
      `main`, once those pages adopt `Detail` per follow-up 9):
      `agents/AgentDetailPage.tsx`, `agents/AutopilotPage.tsx`,
      `analytics/AiAgentsDetail.tsx`, `analytics/ApiTraffic.tsx`,
      `analytics/DimensionList.tsx`, `analytics/EventDetail.tsx`,
      `analytics/PageDetail.tsx`, `analytics/PageFlow.tsx`,
      `analytics/SegmentVisitors.tsx`, `analytics/SessionReplays.tsx`,
      `email/EmailAnalytics.tsx`, `email/EmailDomainsManagement.tsx`,
      `email/EmailsSentList.tsx`, `logs/ProxyLogsList.tsx`,
      `proxy-logs/ProxyLogsDataTable.tsx`,
      `observability/LogExplorer.tsx`, `observability/LogVolume.tsx`,
      `observe/CloudTelemetryActivationSection.tsx`,
      `project/ProjectAnalytics.tsx`, `project/ProjectSpeedInsights.tsx`,
      `visitors/SessionDetail.tsx`, `visitors/VisitorDetail.tsx`,
      `visitors/VisitorsList.tsx`, `pages/BackupDetail.tsx`,
      `pages/S3SourceDetail.tsx`, `pages/ScheduleDetail.tsx`,
      `pages/ScheduleRunDetail.tsx`.
    - **Full pages that are really `Ledger` candidates** (a list is the
      whole page, not embedded — worth a full `Ledger` migration rather
      than a bare `DataTable` swap, similar in spirit to follow-up 9's
      `Detail` migrations): `pages/AiGateway.tsx`, `pages/Alarms.tsx`,
      `pages/AuditLogs.tsx`, `pages/Certificates.tsx`,
      `pages/MetricAlertForm.tsx`, `pages/ProxyMetrics.tsx`,
      `pages/Revenue.tsx`, `pages/ServiceMonitoring.tsx`,
      `pages/ServiceQueryPerformance.tsx`, `pages/ServiceRestore.tsx`,
      `pages/TeamDetail.tsx`, `pages/Teams.tsx`,
      `pages/TraceOperations.tsx`, `pages/TracesList.tsx`,
      `pages/UserDetail.tsx`, `pages/observability/GlobalErrors.tsx`,
      `pages/observability/GlobalTraces.tsx`,
      `pages/settings/NodesPage.tsx`,
      `pages/settings/OtelPipelineStatusPage.tsx`,
      `pages/settings/TraefikDiscoveryPage.tsx`.
    `CronJobDetail.tsx` is also migrated: shared status and formatters,
    independent configuration/history loading, retryable failures, and cached
    data retained after refresh failures. Adjacent regression tests cover these
    states. The remaining sites listed above each need
    its own review for sorting/inline-editing/virtualization behavior that
    a mechanical swap could silently drop.
14. `pages/Projects.tsx`'s card grid was evaluated for migration onto
    `CardGrid` and deliberately **not migrated** — it has batch analytics/
    health/uptime-monitor fetching keyed off the visible page, a bounded
    text-search fallback with its own disclosure copy ("searching N of
    total"), first-run onboarding (`FirstProjectOnboarding`, git-provider
    aware), a migration-source header strip (`PlatformStrip`), and
    `ResponsivePagination` (page-size selector `CardGrid`'s pagination
    footer doesn't support) — enough page-specific logic around the grid
    that a mechanical swap risked behavior regressions for no real
    consolidation win. `CardGrid` ships with a "CardGrid — Projects"
    sandbox reference screen (invented fixtures) instead. Revisit only as
    its own reviewed migration.
    Also re-flagging, explicitly, the two structures called out as
    excluded-by-design for this whole card/grid pass (same framing as the
    log-viewer follow-up 11): `pages/DashboardBuilder.tsx` (626 lines) and
    the `DashboardsRouter.tsx`/`DashboardView.tsx`/`Dashboards.tsx`
    custom-dashboard-builder feature are a real drag-and-drop,
    user-configurable dashboard system — not a `CardGrid`/`Ledger`
    candidate, needs dedicated review if ever touched.
    `components/dashboard/ProjectCard.tsx` (456 lines) is intentionally
    untouched — only the generic grid layout wrapper around it (`CardGrid`)
    was extracted; the card's own internals stay exactly as they are.
15. `SetupWizardShell.tsx` is promoted into `@temps-sdk/ds` as `Wizard`
    (`web/packages/ds/src/wizard.tsx`); its 4 existing importers
    (`ErrorTrackingSetup.tsx`, `ProjectAnalytics.tsx`,
    `AiFirstWorkspace.tsx`, `TracesList.tsx`, plus the
    `harness-onboarding.test.tsx` test) keep working unchanged through the
    thin re-export. At least 9 other wizard-shaped pages hand-roll their
    own step UI instead of adopting `Wizard` — **not migrated this pass**,
    left as a follow-up (each has its own step-count/validation/branching
    logic worth reviewing individually rather than a mechanical swap):
    `AddDomain.tsx`, `ApiKeyCreate.tsx`, `AddDnsProvider.tsx`,
    `AddEmailProvider.tsx`, `AddClusterMember.tsx`,
    `AddNotificationProvider.tsx`, `CreateServiceNew.tsx`,
    `NewProject.tsx`, `Setup.tsx`. `EmailDomainNew.tsx` also hand-rolls its
    own step indicator (a comment there says it took the visual pattern
    from `SetupWizardShell` without importing it) — worth folding into
    `Wizard` alongside the other 9 when this is picked up.

# @temps-sdk/ds — decisions

This is a codification effort, not a redesign. It replaces the cancelled
"operator ink" package (`@temps-sdk/ds` as it existed on `main` before this
branch, née `@temps-sdk/op`, PR #915) — that package had zero consumers in
`web/src` and its `design-system/` sandbox no longer exists on `main`. It was
deleted at the start of this branch and the name is reused for this,
unrelated, effort.

## The six answers

1. **Emotional job**: a developer/operator, often mid-task — configuring,
   deploying, occasionally triaging. Reason: matches CLAUDE.md's stated
   target users (devs/teams wanting Vercel-grade DX); confirmed by user.
   Design favors clarity and speed over decoration.
2. **Signature**: restrained monochrome, one state accent. Reason: extends
   what's already in `web/src/globals.css` — near-black `--primary`, white/
   gray surfaces, color reserved for `--success`/`--warning`/`--destructive`.
   Matches `feedback_temps_brand_colors_white_black` (white/black, not blue).
3. **Colour policy**: state only. Colour never decorates; it appears only
   through the status vocabulary (badges, alerts, chart series). Confirmed
   by user, consistent with the existing token file's intent.
4. **Data shape**: a mix — lists (projects, deployments, logs), single-record
   detail (project/deployment detail), and forms (settings, onboarding/setup
   wizards). Three templates: `Ledger`, `Detail`, `Settings`. Confirmed by
   user; matches `web/src/pages` structure.
5. **Consumers**: this repo only for now — `web/packages/ds`, consumed by
   `web/src` (OSS) and EE within the `temps/` workspace. Kept publish-ready
   (mirrors `@temps-sdk/ui`'s shape) but no cross-repo (vibetemps,
   temps-fleet) commitment yet. Confirmed by user.
6. **Enforcement**: lint only for now — typecheck + a tokens-consistency
   check + a raw-Tailwind/hex-literal audit script on touched files. No
   Playwright visual-baseline suite yet (that was the old op package's
   approach and is more than this phase needs). Confirmed by user.

## Additional scope (from user, mid-build)

- Standardize **page headers** across screens (title/meta/actions row — most
  duplicated, least consistent piece across `web/src/pages` today).
  - Extract as a `PageHeader` primitive.
- Standardize **forms**, including validation/error display and the sticky
  save bar.
- Standardize **onboarding / "not set up yet" pages** for new functionality,
  per the CLAUDE.md rule: unconfigured features must show a surface, say
  what's missing, give a concrete example, and link to the settings page —
  never render nothing. This is the `PageState` primitive's "not set up"
  variant, promoted to a first-class, mandatory pattern rather than a nice-
  to-have.

## Defaults taken without asking (stated, not decided)

- Status vocabulary keeps the existing five-state shape (ok/warn/error/idle/
  running) already implied by `--success`/`--warning`/`--destructive` and
  `AlertStateBadge`/`StatusDot` in `src/components/metrics/alert-format.tsx`,
  rather than inventing new glyphs from scratch. Existing dot/badge visual
  language is kept; the rule work is making it one component instead of
  several near-duplicates.
- Radius: keep `--radius: 0.5rem` (existing token, unchanged).
- Type: keep Geist / Geist Mono (existing tokens, unchanged).
- Consolidate `empty-placeholder.tsx` and `empty-state.tsx` (two overlapping
  components, 101 lines combined) into one `PageState` primitive covering
  empty / not-set-up / failed, per the skill's minimum primitive list.
- Consolidate the ≥14 hand-rolled stat-tile/chart-panel components (
  `MetricTile`, `ThresholdLineChart` usages in `ProjectOverview`,
  `ProjectSpeedInsights`, `ErrorTimeSeriesChart`, `ServerMonitoring`,
  `ApiTraffic`, `PageDetail`, `EventDetail`, `AnalyticsTrafficChart`,
  `MetricsExplorer`, `ProxyMetrics`, `UserDetail`,
  `OtelPipelineStatusPage`) toward one `TimeChart` primitive + a `Lede`/
  stat-row pattern, reusing the existing `recharts`-based
  `threshold-line-chart.tsx` internals rather than rewriting the charting
  engine.
- `useGlobalView.ts` (existing `src/hooks`) is the closest existing URL-state
  hook; `useUrlState` in the package generalizes its pattern rather than
  inventing an unrelated API.
- `kbd-badge.tsx` already exists; the package's `Kbd` wraps/re-exports it
  rather than duplicating it.

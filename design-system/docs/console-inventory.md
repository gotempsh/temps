# Console inventory: what `temps/web` renders today

Survey of `temps/web/src` taken 2026-09-05 to find the data shapes and visual
forms the design system has to cover before the console can be rebuilt on
it. Styling is deliberately ignored; this is about what information each
page shows, how it is structured, and what the reader does with it.

Routing: `App.tsx` (global), `pages/ProjectDetail.tsx` (`/projects/:slug/*`),
`components/project/ProjectAnalytics.tsx` (`analytics/*`). SDK types in
`src/api/client/types.gen.ts` (hey-api) with `*Options()` react-query hooks.

Libraries actually in use: `recharts` 3.10 (only charting lib, wrapped by
`components/ui/chart.tsx`), `react-simple-maps` + bundled `countries-110m.json`
(choropleth), `@react-three/fiber` + `drei` + `three` (3D globe),
`rrweb-player` (session replay), `ansi-to-html` (logs), `monaco-editor` +
`shiki`/`highlight.js` (code), `ghostty-web`/`xterm` (terminals),
`@tanstack/react-virtual`. `@nivo/line` and `cobe` are installed but unused.

## Coverage against the sandbox (`/v1`)

| Area | In web | In sandbox | Gap |
|---|---|---|---|
| Projects, deployments, env vars | list, detail, stages + build logs, runtime logs, env, domains | list, detail, env matrix, deployment record (`deploy:<tag>`) with phased pipeline + per-step logs, build/runtime log facets, checks | virtualised ANSI log viewer, domains/ACME |
| Analytics | overview tiles + hourly chart + 10 breakdown cards with drill-down, dimension list with bars, pages with sparklines, page detail, journey, funnels, replays + player, live globe, visitors/segments/events, AI agents, speed insights + choropleth | metric grid + one chart | breakdown list with bars and drill-down, sparkline in row, funnel, journey transitions, replay player + event timeline, geo (map or globe), score ring, visitor journey |
| Errors | list + stats, occurrences chart, group detail, event detail (stack trace with source context, breadcrumbs, spans, tags, raw JSON), source maps, autofix | issues ledger with status filter, issue record (chart, stack trace, breadcrumbs, latest event, tags, similar), events and tags facets | spans on an event, raw JSON view, source-map upload state, autofix |
| Traces | list, waterfall span tree, span attributes + events, correlated logs, cross-project, operations stats | list, detail with spans (ledger) | collapsible waterfall, correlated log panel |
| Metrics | dashboards grid, explorer with percentiles + histogram, alerts, anomaly band chart, correlations | metrics ledger + chart | percentile selector, histogram panel, band chart, dashboard tile grid, alert rule form |
| Uptime | monitors, bucketed status strip, response-time tiles | nothing (nav only) | status-bar strip with per-bucket popover |
| Proxy | four metric cards, four multi-line charts (status class, destination, error rate, latency percentiles), project filter, ranges | verdict, tile-selected single chart, four Breakdowns, routes ledger, access log | per-route detail, custom range |
| Databases | services, connection info (masked), monitoring charts, slow queries, restore modes incl. PITR, data browser (tree + grid + Monaco), upgrades, cluster members | list, record (`db:<name>`) with health, backups, connect, runs on, danger; facets backups / metrics / logs / queries / data | restore mode selector, data browser is a placeholder, cluster members |
| Backups | sources, schedules, runs, run detail | list | schedule/cron form, run timeline |
| Sandboxes, agents, AI | sandbox detail with events + exec, agent pages, autopilot runs, AI gateway usage/cost, assistant dock | sandboxes, agent chat | usage/cost charts, permission card is covered |
| Settings | users/roles matrix, teams, API keys, SSO/OIDC, security, rate limits, IP access, disk, build limits, timeouts, retention sliders, nodes, registry, plugins, version, domains/certs, DNS, notifications, audit logs, vulnerabilities | git providers, security scans, email; settings hub + 15 pages on a task-based IA (see handoff §7b); nodes ledger with status + node record + cluster page | role matrix, DNS TXT challenge, audit log |

## Visual forms web has that the system did not

Status 2026-09-06: items 1–7 and 9–11 (except the anomaly band chart) are
built in `src/components/op/viz.tsx` and shown on `/op-components`; the
analytics, uptime, metrics and deploys screens on `/v1` use them. Geo is
built as `GeoMap` (choropleth by state, second view of the list). Open:
anomaly band chart, replay player, data browser,
role matrix, retention sliders, DNS challenge block, insight strip.

Ranked by how many pages need them.

1. **Breakdown list with inline percentage bar** and multi-level drill-down
   (country → region → city, browser → version, channel → referrer → page).
   Used by ten analytics cards, dimension lists, page detail, event detail.
2. **Sparkline inside a row or tile** (recharts area in `PageListItem`, CSS
   bars in `Observe` kind tiles, `metric-sparkline`).
3. **Uptime status strip**: one segment per bucket, hover popover with
   counts and p50/p95/p99, legend.
4. **Span waterfall**: collapsible tree, offset/width bars, µs/ms/s, status
   per span, correlated logs beneath.
5. **Stack trace**: frame list, in-app vs vendor, expandable source context
   with gutter, symbolication marker.
6. **Log viewer**: virtualised, ANSI colour, level filter, container
   multi-select, live tail; and the **build-stage stepper** that streams one.
7. **Funnel**: tapering step bars with completions, conversion and drop-off
   per step; horizontal variant; optional value per step.
8. **Geo**: choropleth (`SpeedWorldMap`) and 3D globe with marker overlays
   (`EarthGlobe`). Decision needed (see below).
9. **Score ring** (0–100 arc with tone) for Web Vitals.
10. **Calendar heatmap** for deployment activity.
11. **Percentile selector + histogram distribution** in the metrics explorer;
    **anomaly band chart** (upper/lower area + line + markers).
12. **Session replay player** with a synchronised clickable event timeline.
13. **Journey**: entry, exit, transition "A → B" rows, drop-off list; visitor
    journey as a session timeline with start/end nodes.
14. **Restore mode selector** (in place, new service, PITR with target time).
15. **Data browser**: entity tree, data grid, Monaco editor.
16. **Role/permission matrix**, retention sliders, DNS TXT challenge block,
    insight strip (stat and AI insights with tone and headline value).

## Interaction states that mockups must include

Every list and chart in web has these and a mockup without them is not
buildable:

- empty with setup (OTel wizard, DSN panel, provider missing) — `PageState`
  covers this
- loading and error
- quota/sampled (head-sampled telemetry, plan allowance) — glyph `◌` exists
- time range (quick ranges + custom calendar) — `RangePicker` exists; no
  comparison period exists anywhere in web today
- environment and deployment filters on almost every observe page
- pagination envelope `{data, pagination{page, page_size, total_count,
  total_pages}}` — `Pager` covers this
- live polling (30s monitors, live visitors) — needs a "live" affordance

## Decisions before mocking

- **Geo.** Decided: the ranked list is the default everywhere and carries
  the keyboard; `GeoMap` (react-simple-maps + the same `countries-110m`
  topojson web already bundles) is a second view on by-country lists only,
  filled by state rather than by a value gradient; the WebGL globe leaves
  the console.
- **Charts library.** recharts is the only one in use and the sandbox's
  `TimeChart` is built on it. Keep it; add band, histogram and sparkline
  variants to `TimeChart` rather than a second library.
- **Replay player.** rrweb-player brings its own UI. Decide whether to skin
  it or wrap it with our controls.
- **Data browser and Monaco.** Out of scope for the design system; treat as
  an embedded tool with the shell around it.
- **Comparison period.** Web has none. Decide whether the system should
  offer one before analytics mockups start, since it changes every metric
  tile and chart.

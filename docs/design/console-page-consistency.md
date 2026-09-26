# Console page consistency

Use the existing React/shadcn console design. Preserve its typography, colors, cards, borders, and navigation. Do not migrate to the operator design system.

## Shared page patterns

- Header: page identity and description, followed by creation/import actions.
- List toolbar: visible search, filters, and result context below the header.
- Content: retain the existing cards or tables appropriate to the data. Match loading skeletons to that layout.
- States: distinguish first use, no filter matches, unavailable data, and stale cached data. Keep retry actions beside failures.
- Pagination: use ResponsivePagination. Preserve filters in the URL and reset the page when a filter changes.
- Detail: consistent identity, status, key facts, and supporting sections using the current components.
- Settings: consistent labeled groups and contextual save/error feedback.

## Page-by-page progress

| Page | Status | Work |
| --- | --- | --- |
| Projects | First implementation | Visible shared list toolbar; reloadable search; matching card skeletons; load-error and retry state; preserve existing cards and onboarding |
| Databases / services | Next | Align toolbar, status placement, card facts, loading, and failure states |
| Backups | Next | Align page actions and storage list; distinguish destination configuration from backup success |
| Detail pages | Planned | Align status, facts, section headings, and secondary actions |

Projects searches the first 50 records because the API has no text filter. The toolbar states the searched count and total; all matches within that catalogue are rendered.

Review every changed page at desktop and mobile widths, including populated, empty, loading, and failure states. Use the existing live console, not the separate design sandbox.

## Global observability

The branch now builds on PR #949 (`31da0f449`) for its backend APIs. The existing console style is used at `/analytics`, `/traces`, `/logs`, and `/errors`, with shared headers, project/time filters, explicit errors, and URL state. All four are visible in the platform sidebar and tools directory.

Analytics shows aggregate hourly traffic and a server-paginated list of pages with project identity. Traces link to both project and cross-project waterfalls. Errors link to existing project issue details. Logs use backend cursor pagination; changing a filter invalidates the cursor and scan-budget exhaustion is never labeled as an empty result.

Time windows are persisted as absolute timestamps so reloading and log pagination address the same data. Refresh advances the selected preset window. Project choices are paginated in sets of 100. Database-only log scope disables the project selector because these logs belong to services.


## Compact date/time control

`web/src/components/ui/date-time-range.tsx` provides a controlled `DateTimeRange` component with visible 1h, 6h, 1d, and 7d buttons plus a custom date/time popover. It uses the existing shadcn buttons, inputs, and popover. The popover labels the browser timezone, validates start/end order and the 30-day API limit, and only commits on Apply. Cancel and Escape preserve the applied range.

The global filter bar now places search, project scope, domain filters, and date/time together in a compact wrapping row. Exact timestamps remain accessible in the range description and popover. Custom windows stay fixed when refreshed; quick presets advance to the current time. All range changes clear incompatible page/cursor state.
## Date/time rollout across existing pages

The compact control also replaces the separate quick buttons, dropdowns, and calendars in project analytics (including its subpages and errors), traces and operation rankings, telemetry/request/runtime/service logs, Observe, speed insights, metric exploration and dashboards, proxy metrics, pipeline history, AI usage/activity, monitoring details, funnels, visitor globe/session details, audit logs, and revenue.

`DateRangePicker` now adapts existing Date-based consumers to the same control. `TimeRangeFilter` adapts relative range strings and stores custom ISO bounds together in existing range URL parameters. Legacy preset links retain their meaning. Pages with explicit custom URL fields retain those fields. Backend-specific range limits remain in place. Unfiltered audit/revenue views retain their unbounded state until the user selects a range.

Container resource history shares the four visible quick actions; Custom is disabled with an explanation because `/containers/{container_id}/metrics/history` currently accepts only a preset `range`. Supporting custom bounds there requires extending that API.

Project date-range browser coverage checks exact outgoing quick/custom timestamps, custom persistence after reload, desktop/mobile fit, and trace correlation's intentional omission of time bounds. The global suite checks validation, cancellation, refresh behavior, and cursor reset/persistence. Unit coverage includes custom URL round trips, invalid ranges, and exact elapsed quick durations across daylight-saving transitions.

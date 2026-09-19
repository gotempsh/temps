# Populated console UI audit — 2026-09-19

Read-only review of localhost:3028, proxying the user's populated instance.
Ten routes sampled in light mode at 1440 × 1000; Settings also checked at
390 × 844 with no horizontal document overflow. Screenshots stayed local
because they contain customer data. No settings were saved, providers toggled,
routes reloaded, or resources changed. Login password file was deleted.

This is a prioritized sample, not a complete route inventory or accessibility
audit. Initial loading screenshots were revisited before assessing loaded pages.
Project details, creation flows, dark mode, and mobile pages other than Settings
still need a separate pass. Rankings are design judgments from the rendered UI.

## Next work, in order

| Priority | Route | Observed issue | Proposed change |
|---|---|---|---|
| 1 | /monitoring | Four large shadowed cards, long label-to-switch distances, uneven heights and independent save buttons competing for attention | Aligned section headings and bounded control columns. Preserve independent save scopes, show save feedback beside the affected section; do not silently combine API writes |
| 1 | /settings/request-timeouts | Every primary group is collapsed; no normal page heading in the content | Shared PageHeader and aligned rows; expose common timeout controls and current values, keep override ceilings advanced |
| 1 | /settings/build-limits | Three equal columns contain very unequal amounts of technical explanation; nested notice and outer card dominate | Aligned settings sections. Keep restart requirement and BuildKit applicability visible; move legacy implementation details into named help. Avoid implying unsupported limits are enforced |
| 2 | /settings/notifications | Page title/description followed by another large provider title/description; provider cards repeat their type | One page header with Providers/Routes navigation and the relevant action. Compact provider rows or quieter cards; preserve enabled state and destination identity |
| 2 | /logs | Several toolbar groups compete, tiny dense text, side facets reduce message width; duplicate-looking environment labels appeared | Give search/range primary placement, group presentation/export actions, consider an optional facet panel. Investigate environment identity before merging labels; presentation changes must preserve filtering and wrapping contracts |
| 2 | /monitoring/server | Repeated metric descriptions and chart subtitles, equally heavy cards; capacity forecast is small relative to its significance | Compact overview metrics, concise chart captions, optional sampling help. Keep capacity warnings conspicuous. Do not change sampling/forecast semantics during styling |
| 3 | /projects | Setup strip, migration actions, card graphs and multiple status cues compete with browsing; some project names truncate | Review the surrounding collection toolbar/onboarding priority first. Card internals remain explicitly deferred; don't mechanically migrate this page |
| 3 | /settings/load-balancer | Heading scale differs from adjacent settings pages; list is otherwise concise | Normalize header and action placement; retain the compact route list |
| Keep / light polish | /errors | Search, filters and table are already clear; description is longer than needed | Shorten optional copy and keep the existing collection structure |
| Implemented this pass | /settings | Generic collapsed Troubleshooting hid a named operational action | Visible Route table heading left, short purpose/reload explanation and Reload route table action right; same pattern in sandbox |

## Design-system implications

- Aligned rows are the settings default: section identity left, controls right,
  stacking on mobile. Use spacing rather than a card around every group.
- Show common controls and current values immediately. Collapse genuinely advanced
  configuration, not all content.
- A distinct operation such as route-table reload deserves a named section and
  an explicit action. It is not generic troubleshooting documentation.
- One page header per surface; nested tabs should not introduce another full
  title and description.
- Preserve warnings, scope, units and operational consequences. Reduce background
  explanation through Field.help or named Disclosure.
- Standardize save placement and feedback without changing transaction boundaries.
- Prefer a dedicated review for log presentation and complex project cards.

## Route-table verification

Checked the existing backend handler: POST /settings/routes/refresh reloads saved
routes into the proxy's in-memory cache and returns the loaded route count.
The UI retains that existing request; this pass changes its discoverability and
copy. The live action was intentionally not invoked. Console TypeScript, DS lint,
and sandbox build passed; browser confirmed the named section and exposed action.

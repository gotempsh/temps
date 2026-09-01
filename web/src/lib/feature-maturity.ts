// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type FeatureMaturityTier = 'stable' | 'beta' | 'experimental'

export interface FeatureMaturity {
  key: string
  maturity: FeatureMaturityTier
  reason: string
  docs_path: string
}

export const BETA_TOOLTIP =
  'Available for use. The data model and API may change between releases.'
export const EXPERIMENTAL_TOOLTIP =
  "Under active development. May change substantially or be removed. Don't build on it yet."

/** Resolve the most specific feature represented by an application route. */
export function featureKeyForPath(pathname: string): string | undefined {
  const projectRoute = pathname.match(/^\/projects\/[^/]+\/(.*)$/)?.[1] ?? ''

  if (projectRoute.startsWith('analytics/replays')) return 'session-replay'
  if (projectRoute.startsWith('analytics/funnels')) return 'funnels'
  if (projectRoute.startsWith('analytics')) return 'web-analytics'
  if (projectRoute.startsWith('errors')) return 'error-tracking'
  if (
    projectRoute.startsWith('traces') ||
    projectRoute.startsWith('metrics') ||
    projectRoute.startsWith('telemetry-logs')
  ) {
    return 'otel-traces-metrics'
  }
  if (projectRoute.startsWith('speed')) return 'performance-monitoring'
  if (projectRoute.startsWith('revenue')) return 'revenue-tracking'
  if (projectRoute.startsWith('agents')) return 'ai-agents-workflows'
  if (projectRoute.startsWith('ai-gateway')) return 'ai-gateway'
  if (projectRoute.includes('database-browser')) return 'database-browser'

  if (pathname.startsWith('/monitoring')) return 'alerts-metric-alerts'
  if (pathname.startsWith('/sandboxes')) return 'sandboxes-preview-environments'
  if (pathname.startsWith('/settings/teams')) return 'teams'
  if (pathname.startsWith('/settings/nodes')) return 'multi-node-worker-join'
  if (pathname.startsWith('/settings/plugins')) return 'plugin-system'
  if (pathname.startsWith('/ai-gateway')) return 'ai-gateway'
  if (pathname.startsWith('/chat')) return 'ai-chat'
  if (pathname.startsWith('/ai-workflows')) return 'ai-agents-workflows'
  if (pathname.startsWith('/agent-sandbox')) return 'agent-execution-sandbox'
  if (pathname.startsWith('/revenue')) return 'revenue-tracking'
  if (pathname.startsWith('/status-pages')) return 'status-pages'
  if (pathname.startsWith('/screenshots')) return 'screenshots'
  if (pathname.startsWith('/security')) return 'vulnerability-scanning'

  return undefined
}

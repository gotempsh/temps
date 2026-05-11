/**
 * Build the URL for a "View all" dimension page while preserving the current
 * date-filter query params (filter / from / to) from the overview.
 */
export function buildAnalyticsDimensionUrl(
  projectSlug: string,
  dimension: string,
  searchParams: URLSearchParams
): string {
  const params = new URLSearchParams()
  const filter = searchParams.get('filter')
  const from = searchParams.get('from')
  const to = searchParams.get('to')
  if (filter) params.set('filter', filter)
  if (from) params.set('from', from)
  if (to) params.set('to', to)
  const qs = params.toString()
  return `/projects/${projectSlug}/analytics/dimensions/${dimension}${qs ? `?${qs}` : ''}`
}

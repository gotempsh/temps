export interface ProxyLogDrilldown {
  projectId: number
  clientIp: string
  method: string
  path: string
  startDate: string
  endDate: string
  environmentId?: number
}

/**
 * Build a project-scoped Proxy Logs URL that preserves the exact analytics
 * window. The global Proxy Logs table includes bot traffic unless `is_bot` is
 * explicitly set, matching API Traffic's default scope.
 */
export function apiTrafficProxyLogsUrl({
  projectId,
  clientIp,
  method,
  path,
  startDate,
  endDate,
  environmentId,
}: ProxyLogDrilldown): string {
  const params = new URLSearchParams({
    project_id: String(projectId),
    client_ip: clientIp,
    method,
    path_exact: path,
    exclude_synthetic: 'true',
    start_date: startDate,
    end_date: endDate,
    time_range: 'custom',
    filters: 'open',
  })
  if (environmentId !== undefined) {
    params.set('environment_id', String(environmentId))
  }

  return `/proxy-logs?${params.toString()}`
}

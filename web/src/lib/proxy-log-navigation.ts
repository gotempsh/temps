export function proxyLogDetailUrl({
  requestId,
  timestamp,
  projectId,
}: {
  requestId: string
  timestamp: string
  projectId?: number | null
}): string {
  const params = new URLSearchParams({ ts: timestamp })
  if (projectId !== undefined && projectId !== null) {
    params.set('project_id', String(projectId))
  }
  return `/proxy-logs/${encodeURIComponent(requestId)}?${params.toString()}`
}

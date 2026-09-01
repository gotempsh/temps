// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

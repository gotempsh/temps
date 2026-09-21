// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { listHttpChecks, type HttpCheckView } from '@/api/client'

export const httpChecksKey = (projectId: number) => ['http-checks', projectId]
export function useHttpChecks(projectId: number) {
  return useQuery({
    queryKey: httpChecksKey(projectId),
    queryFn: async () => {
      const items: HttpCheckView[] = []
      let page = 1
      while (true) {
        const response = await listHttpChecks({
          path: { project_id: projectId },
          query: { page, page_size: 100 },
          throwOnError: true,
        })
        items.push(...response.data.items)
        if (
          items.length >= response.data.total ||
          response.data.items.length === 0
        )
          return items
        page++
      }
    },
    refetchInterval: 10_000,
  })
}

export function checkIndicators(checks: HttpCheckView[]) {
  return checks.map((check) => ({
    id: String(check.id),
    status: !check.enabled
      ? ('unknown' as const)
      : (check.result?.status ?? ('pending' as const)),
    label: !check.enabled
      ? `${check.name}: paused`
      : check.result
        ? `${check.name}: ${check.result.status}`
        : `${check.name}: awaiting check`,
    detail: !check.enabled
      ? 'Scheduled checks are paused.'
      : check.result
        ? `${check.result.findings.map((finding) => finding.message).join(' ')} Last checked ${new Date(check.result.checked_at).toLocaleString()}.`
        : 'The first HTTP check has not completed yet.',
  }))
}

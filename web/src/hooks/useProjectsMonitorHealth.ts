// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectsMonitorHealthOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

/**
 * Uptime-monitor health for a batch of projects.
 *
 * This is the authoritative "is it up" signal: it reads the latest production
 * status check per project, so it answers even for a project nobody visited.
 * Proxy-log health (`useDashboardHealth`) cannot — it measures *user traffic*
 * and explicitly excludes Temps' own monitor requests, so a healthy but quiet
 * project has nothing to derive a status from.
 */
export function useProjectsMonitorHealth(projectIds: number[]) {
  return useQuery({
    ...getProjectsMonitorHealthOptions({
      query: { project_ids: projectIds.join(',') },
    }),
    enabled: projectIds.length > 0,
    staleTime: 1000 * 30,
    refetchInterval: 1000 * 30,
    // Monitors are an optional feature and the endpoint is permission-gated
    // (StatusPageRead). A user who cannot read them should fall back to
    // traffic health, not retry a guaranteed 403 three times per project list.
    retry: false,
  })
}

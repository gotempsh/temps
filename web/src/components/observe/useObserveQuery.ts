// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { observabilityListEventsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import type { EventKind } from './types'
import { ALL_KINDS } from './types'

interface UseObserveQueryArgs {
  projectId: number
  kinds: EventKind[]
  from?: Date
  to?: Date
  deploymentId?: number
  environmentId?: number
  search?: string
  limit?: number
  hideBots?: boolean
}

/**
 * Wraps the generated `observabilityListEventsOptions` so call-sites can
 * pass typed JS values (Date, EventKind[]) instead of the wire format
 * (ISO strings + comma-separated kinds).
 */
export function useObserveQuery(args: UseObserveQueryArgs) {
  const {
    projectId,
    kinds,
    from,
    to,
    deploymentId,
    environmentId,
    search,
    limit,
    hideBots,
  } = args

  // Omit `kinds` when the user has every kind selected so the server
  // returns its full default — this also keeps the URL state clean.
  const kindsParam =
    kinds.length === ALL_KINDS.length || kinds.length === 0
      ? undefined
      : kinds.slice().sort().join(',')

  return useQuery({
    ...observabilityListEventsOptions({
      path: { project_id: projectId },
      query: {
        kinds: kindsParam,
        from: from?.toISOString(),
        to: to?.toISOString(),
        deployment_id: deploymentId,
        environment_id: environmentId,
        search: search || undefined,
        limit,
        // Only send `hide_bots` when actively filtering out bots — leaving
        // it unset means "include everything" so we keep URL/query state
        // minimal in the default-on-bots case.
        hide_bots: hideBots ? true : undefined,
      },
    }),
    refetchInterval: 15_000,
  })
}

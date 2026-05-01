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
      },
    }),
    refetchInterval: 15_000,
  })
}

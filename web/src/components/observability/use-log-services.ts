// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  useServiceScope,
  type ScopedService,
} from '@/contexts/ServiceScopeContext'

/** How many services one request lists; past it the picker says so. */
const LOG_SERVICE_PAGE = 100

export interface LogServices {
  services: ScopedService[]
  /** More services exist than are listed. */
  truncated: boolean
  isPending: boolean
  failed: boolean
  retry: () => void
}

/**
 * The services a log control can be pointed at: the project's own inside a
 * project, every service on the server otherwise. Asks nothing until
 * `enabled`, so a closed popover or dialog costs no request.
 */
export function useLogServices(enabled: boolean): LogServices {
  const scope = useServiceScope()
  const projects = useQuery({
    ...getProjectsOptions({
      query: { per_page: LOG_SERVICE_PAGE, page: 1 },
    }),
    enabled: enabled && !scope,
    staleTime: 60_000,
  })
  if (scope)
    return {
      services: scope.services,
      truncated: false,
      isPending: false,
      failed: false,
      retry: () => {},
    }
  const listed = projects.data?.projects ?? []
  return {
    services: listed,
    truncated: (projects.data?.total ?? 0) > listed.length,
    isPending: projects.isPending,
    failed: projects.isError,
    retry: () => void projects.refetch(),
  }
}

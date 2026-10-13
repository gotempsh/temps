// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { RoutesManagement } from '@/components/routes/RoutesManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { listRoutesOptions } from '@/api/client/@tanstack/react-query.gen'

export function Routes() {
  const { setBreadcrumbs } = useBreadcrumbs()

  // The generated options throw the server's Problem Details on a non-2xx
  // response. A bare `listRoutes()` resolves with `{ error }` instead, so a
  // failed read used to look like a successful read of zero routes.
  const {
    data: routes,
    isLoading,
    isError,
    error,
    isFetching,
    refetch: refetchRoutes,
  } = useQuery({
    ...listRoutesOptions(),
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Load Balancer', href: '/settings/load-balancer' },
      { label: 'Routes' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Routes')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <RoutesManagement
          routes={routes}
          isLoading={isLoading}
          error={isError ? error : undefined}
          retrying={isFetching}
          reloadRoutes={refetchRoutes}
        />
      </div>
    </div>
  )
}

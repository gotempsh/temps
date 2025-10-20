import { RoutesManagement } from '@/components/routes/RoutesManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { listDomains, listRoutes } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { AlertCircle } from 'lucide-react'

export function Routes() {
  const { setBreadcrumbs } = useBreadcrumbs()

  const {
    data: routes,
    isLoading: isRoutesLoading,
    error: routesError,
    refetch: refetchRoutes,
  } = useQuery({
    queryKey: ['routes'],
    queryFn: () => listRoutes(),
  })

  const {
    data: domains,
    isLoading: isDomainsLoading,
    error: domainsError,
  } = useQuery({
    queryKey: ['active-domains'],
    queryFn: () => listDomains(),
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Load Balancer', href: '/load-balancer' },
      { label: 'Routes' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Routes')

  if (routesError || domainsError) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>
          Failed to load routes data. Please try again later or contact support
          if the issue persists.
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <RoutesManagement
          routes={routes?.data || []}
          domains={domains?.data}
          isLoading={isRoutesLoading || isDomainsLoading}
          reloadRoutes={refetchRoutes}
        />
      </div>
    </div>
  )
}

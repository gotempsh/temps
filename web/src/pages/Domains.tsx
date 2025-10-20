import { useQuery } from '@tanstack/react-query'
import { listDomainsOptions } from '@/api/client/@tanstack/react-query.gen'
import { DomainsManagement } from '@/components/domains/DomainsManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'

export function Domains() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const {
    data: domains,
    isLoading,
    refetch,
  } = useQuery({
    ...listDomainsOptions({}),
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Domains' }])
  }, [setBreadcrumbs])

  usePageTitle('Domains')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <DomainsManagement
          domains={domains?.domains || []}
          isLoading={isLoading}
          reloadDomains={refetch}
        />
      </div>
    </div>
  )
}

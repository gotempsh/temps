import { ProvidersManagement } from '@/components/monitoring/ProvidersManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'

export function Notifications() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Notifications' }])
  }, [setBreadcrumbs])

  usePageTitle('Notifications')

  return (
    <div className="w-full px-4 sm:px-6 lg:px-8 py-8">
      <div className="max-w-7xl mx-auto">
        <ProvidersManagement />
      </div>
    </div>
  )
}

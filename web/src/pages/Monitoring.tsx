import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { Outlet } from 'react-router-dom'

export function Monitoring() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Monitoring & Alerts' }])
  }, [setBreadcrumbs])

  usePageTitle('Monitoring & Alerts')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <Outlet />
      </div>
    </div>
  )
}

import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { monitoringSectionLabel } from '@/components/monitoring/monitoring-sections'
import { useEffect } from 'react'
import { Outlet, useLocation } from 'react-router'

export function Monitoring() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const location = useLocation()
  const sectionId = location.pathname.split('/')[2] || 'alerts'
  const sectionLabel = monitoringSectionLabel(sectionId)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Monitoring & Alerts', href: '/monitoring/alerts' },
      ...(sectionLabel ? [{ label: sectionLabel }] : []),
    ])
  }, [sectionLabel, setBreadcrumbs])

  usePageTitle('Monitoring & Alerts')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <Outlet />
      </div>
    </div>
  )
}

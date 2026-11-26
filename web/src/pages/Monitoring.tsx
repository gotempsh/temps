// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  DEFAULT_MONITORING_SECTION,
  monitoringSectionLabel,
} from '@/components/monitoring/monitoring-sections'
import { ALERTS_URL } from '@/lib/server-navigation'
import { useEffect } from 'react'
import { Outlet, useLocation } from 'react-router'
import { PageContainer } from '@/components/layout/PageContainer'

export function Monitoring() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const location = useLocation()
  const sectionId =
    location.pathname.split('/')[2] || DEFAULT_MONITORING_SECTION
  const sectionLabel = monitoringSectionLabel(sectionId)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Alerts', href: ALERTS_URL },
      ...(sectionLabel ? [{ label: sectionLabel }] : []),
    ])
  }, [sectionLabel, setBreadcrumbs])

  usePageTitle('Alerts')

  return (
    <div className="flex-1 overflow-auto">
      <PageContainer innerClassName="space-y-6">
        <Outlet />
      </PageContainer>
    </div>
  )
}

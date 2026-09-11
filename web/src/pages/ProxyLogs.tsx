// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer, PageHeader } from '@/components/layout/PageContainer'

import { useEffect } from 'react'
import { ProxyLogsDataTable } from '@/components/proxy-logs/ProxyLogsDataTable'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function ProxyLogs() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy Logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy Logs')

  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Proxy Logs"
        description="Advanced proxy request logs with comprehensive filtering and sorting"
      />
      <ProxyLogsDataTable />
    </PageContainer>
  )
}

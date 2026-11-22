// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer, PageHeader } from '@/components/layout/PageContainer'

import { useEffect, useState } from 'react'
import { getProxyLogsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { ProxyLogsDataTable } from '@/components/proxy-logs/ProxyLogsDataTable'
import { RefreshButton } from '@/components/ui/refresh-button'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function ProxyLogs() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [refreshedAt, setRefreshedAt] = useState<number>()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy logs')

  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Proxy logs"
        description="Advanced proxy request logs with comprehensive filtering and sorting"
        actions={
          <RefreshButton
            queryKeys={[getProxyLogsQueryKey()]}
            onRefresh={() => setRefreshedAt(Date.now())}
          />
        }
      />
      <ProxyLogsDataTable refreshedAt={refreshedAt} />
    </PageContainer>
  )
}

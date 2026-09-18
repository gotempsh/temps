// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { PageContainer } from '@temps-sdk/ds'
import { ServerMonitoring } from '@/components/monitoring/ServerMonitoring'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export function Server() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Server')
  useEffect(() => {
    setBreadcrumbs([{ label: 'Server', href: '/monitoring/server' }])
  }, [setBreadcrumbs])
  return (
    <PageContainer>
      <ServerMonitoring />
    </PageContainer>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer } from '@/components/layout/PageContainer'
import { StatusPagesList } from '@/components/project/status-pages/ProjectStatusPages'
import { StatusPageDetail } from '@/components/project/status-pages/StatusPageDetail'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'

/** `/status-pages`: every status page, whichever projects it shows. */
export function StatusPages() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Status pages')
  useEffect(() => {
    setBreadcrumbs([{ label: 'Status pages' }])
  }, [setBreadcrumbs])

  return (
    <PageContainer innerClassName="space-y-6">
      <StatusPagesList />
    </PageContainer>
  )
}

/** `/status-pages/:statusPageId`. */
export function StatusPageDetailPage() {
  return (
    <PageContainer innerClassName="space-y-6">
      <StatusPageDetail />
    </PageContainer>
  )
}

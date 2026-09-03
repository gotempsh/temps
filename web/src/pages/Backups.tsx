// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { S3SourcesManagement } from '@/components/backups/S3SourcesManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'

export function Backups() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Backups' }])
  }, [setBreadcrumbs])

  usePageTitle('Backups')

  // Backup alerts are surfaced globally via the header's `BackupAlertsButton`
  // so operators see overdue schedules / stalled jobs from any page.
  return (
    <div className="flex-1 overflow-auto">
      <PageContainer innerClassName="space-y-6">
        <PageHeader
          title="Backups"
          description="Configure where backups and WAL archives are stored"
        />
        <S3SourcesManagement />
      </PageContainer>
    </div>
  )
}

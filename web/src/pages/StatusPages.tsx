// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { CreateStatusPageForm } from '@/components/project/status-pages/CreateStatusPageForm'
import { StatusPagesList } from '@/components/project/status-pages/ProjectStatusPages'
import { StatusPageDetail } from '@/components/project/status-pages/StatusPageDetail'
import { parseInitialProjectId } from '@/components/project/status-pages/status-page-form'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useSearchParams } from 'react-router'

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

/** `/status-pages/new`: create a page, on the project given by `?project=`. */
export function NewStatusPagePage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [searchParams] = useSearchParams()
  usePageTitle('Create status page')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Status pages', href: '/status-pages' },
      { label: 'Create status page' },
    ])
  }, [setBreadcrumbs])

  return (
    <PageContainer width="form" innerClassName="space-y-6">
      <PageHeader
        title="Create status page"
        description="A public page showing whether your services are up, plus the incidents you post."
      />
      <CreateStatusPageForm
        initialProjectId={parseInitialProjectId(searchParams.get('project'))}
      />
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

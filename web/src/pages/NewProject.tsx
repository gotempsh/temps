import { useEffect } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { GitImportClone } from '@/components/project/GitImportClone'
import { PageContainer } from '@/components/layout/PageContainer'

export function NewProject() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'New Project' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('New Project')

  return (
    <PageContainer>
      <GitImportClone mode="navigation" />
    </PageContainer>
  )
}

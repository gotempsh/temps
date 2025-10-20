import { useEffect } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { GitImportClone } from '@/components/project/GitImportClone'

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
    <div className="container mx-auto py-10">
      <div className="flex flex-col gap-6 md:flex-row">
        <GitImportClone mode="navigation" />
      </div>
    </div>
  )
}

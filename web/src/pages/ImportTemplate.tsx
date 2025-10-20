'use client'

import {
  // getRepoSourcesOptions, // API no longer exists
  getTemplateByNameOptions,
  listServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ImportTemplateForm } from '@/components/templates/ImportTemplateForm'
import { Button } from '@/components/ui/button'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { useEffect } from 'react'
import { useNavigate, useParams } from 'react-router-dom'

export function ImportTemplate() {
  const { name } = useParams<{ name: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'New Project', href: '/projects/new' },
      { label: 'Import Template' },
    ])
  }, [setBreadcrumbs])

  // TODO: Replace with new Git Provider API
  const sourcesData = null
  const sourcesLoading = false

  const { data: templateData, isLoading: templateLoading } = useQuery({
    ...getTemplateByNameOptions({
      path: {
        name: name!,
      },
    }),
    enabled: !!name,
  })

  usePageTitle(`Import ${templateData?.name || name} Template`)

  const {
    data: storageServicesData,
    isLoading: isStorageServicesLoading,
    refetch: reloadServices,
  } = useQuery({
    ...listServicesOptions({}),
  })

  const isLoading =
    templateLoading || sourcesLoading || isStorageServicesLoading

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-screen">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  if (!templateData) {
    return (
      <div className="flex items-center justify-center min-h-screen">
        <div className="text-center">
          <h2 className="text-2xl font-bold">Template not found</h2>
          <Button className="mt-4" onClick={() => navigate(-1)}>
            Go Back
          </Button>
        </div>
      </div>
    )
  }

  return (
    <ImportTemplateForm
      template={templateData}
      sources={sourcesData || []}
      storageServices={storageServicesData || []}
      reloadServices={reloadServices}
    />
  )
}

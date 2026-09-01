// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { listContainersOptions } from '@/api/client/@tanstack/react-query.gen'
import { ContainerActionDialog } from '@/components/containers/ContainerActionDialog'
import { ContainerDetail } from '@/components/containers/ContainerDetail'
import { ContainerHeaderBar } from '@/components/containers/ContainerHeaderBar'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft } from 'lucide-react'
import { useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'

interface ContainerDetailPageProps {
  project: ProjectResponse
}

export function ContainerDetailPage({ project }: ContainerDetailPageProps) {
  const { containerId } = useParams<{ containerId: string }>()
  const [searchParams, setSearchParams] = useSearchParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const environmentId = searchParams.get('env') || ''
  const rawTab = searchParams.get('tab')
  const selectedTab: 'logs' | 'configuration' =
    rawTab === 'configuration' ? 'configuration' : 'logs'

  const [actionType, setActionType] = useState<
    'start' | 'stop' | 'restart' | null
  >(null)

  const { data, isLoading } = useQuery({
    ...listContainersOptions({
      path: {
        project_id: project.id,
        environment_id: parseInt(environmentId || '0'),
      },
    }),
    enabled: !!environmentId,
    staleTime: 5000,
  })

  const containers = data?.containers ?? []
  const selectedContainer =
    containers.find((c) => c.container_id === containerId) ?? null

  const handleTabChange = (tab: 'logs' | 'configuration') => {
    searchParams.set('tab', tab)
    setSearchParams(searchParams, { replace: true })
  }

  const handleSelectContainer = (id: string) => {
    navigate(
      `/projects/${project.slug}/environments/containers/${id}?env=${environmentId}&tab=${selectedTab}`
    )
  }

  const backHref = `/projects/${project.slug}/environments`

  if (!environmentId) {
    return (
      <div className="p-6">
        <p className="text-sm text-muted-foreground">
          Missing environment. <Link to={backHref}>Go back</Link>.
        </p>
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex flex-col h-full">
        <div className="w-full px-4 sm:px-6 lg:px-8 pt-5">
          <Skeleton className="h-5 w-48 mb-4" />
          <Skeleton className="h-10 w-72" />
        </div>
      </div>
    )
  }

  if (!selectedContainer) {
    return (
      <div className="w-full px-4 sm:px-6 lg:px-8 pt-5">
        <Link
          to={backHref}
          className="inline-flex items-center gap-1.5 text-sm text-neutral-500 hover:text-neutral-900 dark:text-neutral-400 dark:hover:text-white"
        >
          <ArrowLeft className="size-4" aria-hidden="true" />
          Containers
        </Link>
        <div className="mt-6 flex flex-col items-center justify-center h-72 rounded-lg border border-neutral-950/10 bg-neutral-50 p-6 dark:border-white/10 dark:bg-white/5">
          <p className="text-sm font-semibold text-neutral-900 dark:text-white">
            Container not found
          </p>
          <p className="mt-1 text-sm text-neutral-600 dark:text-neutral-400">
            It may have been removed or it belongs to a different environment.
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full bg-white dark:bg-neutral-950">
      <div className="w-full px-4 sm:px-6 lg:px-8 pt-5">
        <Link
          to={backHref}
          className="inline-flex items-center gap-1.5 text-sm text-neutral-500 hover:text-neutral-900 dark:text-neutral-400 dark:hover:text-white"
        >
          <ArrowLeft className="size-4" aria-hidden="true" />
          Containers
        </Link>
      </div>

      <ContainerHeaderBar
        projectId={project.id.toString()}
        environmentId={environmentId}
        containers={containers}
        selectedContainer={selectedContainer}
        onSelect={handleSelectContainer}
        tab={selectedTab}
        onTabChange={handleTabChange}
        onAction={setActionType}
      />

      <div className="flex-1 overflow-auto">
        {selectedTab === 'logs' ? (
          <ContainerDetail
            projectId={project.id.toString()}
            environmentId={environmentId}
            containerId={selectedContainer.container_id}
            tab={selectedTab}
          />
        ) : (
          <div className="w-full px-4 py-6 sm:px-6 sm:py-8 lg:px-8">
            <ContainerDetail
              projectId={project.id.toString()}
              environmentId={environmentId}
              containerId={selectedContainer.container_id}
              tab={selectedTab}
            />
          </div>
        )}
      </div>

      <ContainerActionDialog
        projectId={project.id.toString()}
        environmentId={environmentId}
        action={actionType}
        containerId={selectedContainer.container_id}
        onClose={() => setActionType(null)}
        onSuccess={() => {
          queryClient.invalidateQueries({
            queryKey: listContainersOptions({
              path: {
                project_id: project.id,
                environment_id: parseInt(environmentId),
              },
            }).queryKey,
          })
        }}
      />
    </div>
  )
}

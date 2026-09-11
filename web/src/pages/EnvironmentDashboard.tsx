// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironmentOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { ContainerList } from '@/components/containers/ContainerList'
import { ContainerActionDialog } from '@/components/containers/ContainerActionDialog'
import { EnvironmentSettingsContent } from '@/components/environments/EnvironmentSettingsContent'
import { EnvironmentNavigation } from '@/components/environments/EnvironmentNavigation'
import { EnvironmentHeaderBar } from '@/components/environments/EnvironmentHeaderBar'
import { EnvironmentMetricsCharts } from '@/components/monitoring/EnvironmentMetricsCard'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useSearchParams } from 'react-router'
import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { useCallback, useState } from 'react'
import {
  resolveEnvironmentView,
  updateEnvironmentSearchParams,
  type EnvironmentView,
} from '@/lib/environment-navigation'

interface EnvironmentDashboardProps {
  project: ProjectResponse
  environmentId: number
  environments?: EnvironmentResponse[]
  onEnvironmentChange?: (id: number) => void
  onCreateEnvironment?: () => void
  onDelete?: () => void
}

export function EnvironmentDashboard({
  project,
  environmentId,
  environments,
  onEnvironmentChange,
  onCreateEnvironment,
  onDelete,
}: EnvironmentDashboardProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const activeView = resolveEnvironmentView(searchParams.get('view'))

  const handleViewChange = useCallback(
    (view: EnvironmentView) => {
      const nextParams = updateEnvironmentSearchParams(searchParams, { view })
      setSearchParams(nextParams)
    },
    [searchParams, setSearchParams]
  )

  const {
    data: environment,
    isLoading: isEnvironmentLoading,
    error: environmentError,
    refetch,
  } = useQuery({
    ...getEnvironmentOptions({
      path: {
        project_id: project?.id || 0,
        env_id: environmentId,
      },
    }),
    enabled: !!project?.id && !!environmentId,
  })

  if (environmentError) {
    return (
      <div className="p-4 sm:p-6">
        <ErrorAlert
          title="Failed to load environment"
          description={
            environmentError instanceof Error
              ? environmentError.message
              : 'An unexpected error occurred'
          }
          retry={() => refetch()}
        />
      </div>
    )
  }

  if (isEnvironmentLoading) {
    return <EnvironmentDashboardSkeleton />
  }

  if (!environment) {
    return (
      <div className="p-4 sm:p-6">
        <ErrorAlert
          title="Environment not found"
          description="The environment you're looking for does not exist"
          retry={() => refetch()}
        />
      </div>
    )
  }

  const isStatic = project?.source_type === 'static_files'

  return (
    <div className="grid min-w-0 flex-1 content-start bg-background lg:content-stretch lg:grid-cols-[200px_minmax(0,1fr)]">
      <EnvironmentNavigation
        environment={environment}
        activeView={activeView}
        onViewChange={handleViewChange}
        environments={environments}
        onEnvironmentChange={onEnvironmentChange}
        onCreateEnvironment={onCreateEnvironment}
      />
      <div className="min-w-0">
        <EnvironmentHeaderBar environment={environment} project={project} />
        <div className="w-full px-4 py-6 sm:px-6 sm:py-8 lg:px-8">
          {activeView === 'settings' ? (
            <EnvironmentSettingsContent
              environment={environment}
              project={project}
              environmentId={environmentId.toString()}
              onDelete={onDelete}
            />
          ) : isStatic ? (
            <div className="flex flex-col items-center justify-center h-72 rounded-lg border border-neutral-950/10 bg-neutral-50 p-6 text-center dark:border-white/10 dark:bg-white/5">
              <p className="text-sm font-semibold text-neutral-900 dark:text-white">
                Static site
              </p>
              <p className="mt-1 text-sm text-neutral-600 dark:text-neutral-400">
                This project does not have running containers to manage.
              </p>
            </div>
          ) : activeView === 'metrics' ? (
            <EnvironmentMetricsCharts
              projectId={project.id}
              environmentId={environmentId}
            />
          ) : (
            <ContainerPanel
              project={project}
              environmentId={environmentId.toString()}
            />
          )}
        </div>
      </div>
    </div>
  )
}

export function EnvironmentDashboardSkeleton() {
  return (
    <div
      className="grid min-w-0 flex-1 content-start lg:grid-cols-[200px_minmax(0,1fr)] lg:content-stretch"
      aria-label="Loading environment"
    >
      <div className="border-b p-4 lg:border-b-0 lg:border-r lg:px-3 lg:py-5">
        <Skeleton className="h-16 w-full" />
        <div className="mt-5 hidden space-y-2 lg:block">
          {[0, 1, 2].map((item) => (
            <Skeleton key={item} className="h-9 w-full" />
          ))}
        </div>
      </div>
      <div className="min-w-0">
        <div className="space-y-3 border-b px-4 py-5 sm:px-6 lg:px-8">
          <Skeleton className="h-7 w-48" />
          <Skeleton className="h-4 w-56" />
        </div>
        <div className="px-4 py-6 sm:px-6 sm:py-8 lg:px-8">
          <Skeleton className="h-24 w-full" />
        </div>
      </div>
    </div>
  )
}

interface ContainerPanelProps {
  project: ProjectResponse
  environmentId: string
}

function ContainerPanel({ project, environmentId }: ContainerPanelProps) {
  const queryClient = useQueryClient()
  const [action, setAction] = useState<{
    containerId: string
    type: 'start' | 'stop' | 'restart'
  } | null>(null)

  return (
    <>
      <ContainerList
        project={project}
        environmentId={environmentId}
        onAction={(containerId, type) => setAction({ containerId, type })}
      />
      <ContainerActionDialog
        projectId={project.id.toString()}
        environmentId={environmentId}
        action={action?.type ?? null}
        containerId={action?.containerId ?? null}
        onClose={() => setAction(null)}
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
    </>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageTitle } from '@temps-sdk/ds'
import {
  getEnvironmentOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  Boxes,
  LineChart,
  Plus,
  Settings2,
  Shield,
  type LucideIcon,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { ContainerList } from '@/components/containers/ContainerList'
import { ContainerActionDialog } from '@/components/containers/ContainerActionDialog'
import { EnvironmentSettingsContent } from '@/components/environments/EnvironmentSettingsContent'
import { EnvironmentMetricsCharts } from '@/components/monitoring/EnvironmentMetricsCard'
import { EnvironmentShieldView } from '@/components/environments/shield/EnvironmentShieldView'
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
  onCreateEnvironment?: () => void
  onDelete?: () => void
}

/**
 * One environment's pages as tabs. The project bar picks the environment and
 * shows its name, so the page names itself for screen readers only.
 */

export function EnvironmentDashboard({
  project,
  environmentId,
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
      <div>
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
      <div>
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
    <div className="min-w-0 space-y-6">
      {/* The project bar already shows the environment's name. */}
      <PageTitle className="sr-only">{environment.name}</PageTitle>
      <div className="flex items-center gap-3 border-b">
        <Tabs
          className="min-w-0 flex-1"
          value={activeView}
          onValueChange={(value) =>
            handleViewChange(resolveEnvironmentView(value))
          }
        >
          <TabsList
            className="border-b-0"
            aria-label={`${environment.name} pages`}
          >
            {ENVIRONMENT_VIEWS.map((view) => (
              <TabsTrigger key={view.value} value={view.value} icon={view.icon}>
                {view.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
        {onCreateEnvironment && (
          <Button
            variant="outline"
            size="sm"
            className="shrink-0"
            onClick={onCreateEnvironment}
          >
            <Plus className="size-4" aria-hidden="true" />
            <span className="max-sm:sr-only">New environment</span>
          </Button>
        )}
      </div>
      <EnvironmentViewContent
        project={project}
        environment={environment}
        environmentId={environmentId}
        activeView={activeView}
        isStatic={isStatic}
        onViewChange={handleViewChange}
        onDelete={onDelete}
      />
    </div>
  )
}

const ENVIRONMENT_VIEWS: {
  value: EnvironmentView
  label: string
  icon: LucideIcon
}[] = [
  { value: 'containers', label: 'Containers', icon: Boxes },
  { value: 'metrics', label: 'Metrics', icon: LineChart },
  { value: 'shield', label: 'Shield', icon: Shield },
  { value: 'settings', label: 'Settings', icon: Settings2 },
]

function EnvironmentViewContent({
  project,
  environment,
  environmentId,
  activeView,
  isStatic,
  onViewChange,
  onDelete,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
  environmentId: number
  activeView: EnvironmentView
  isStatic: boolean
  onViewChange: (view: EnvironmentView) => void
  onDelete?: () => void
}) {
  return activeView === 'settings' ? (
    <EnvironmentSettingsContent
      environment={environment}
      project={project}
      environmentId={environmentId.toString()}
      onDelete={onDelete}
    />
  ) : activeView === 'shield' ? (
    <EnvironmentShieldView
      project={project}
      environment={environment}
      onOpenSettings={() => onViewChange('settings')}
    />
  ) : isStatic ? (
    <div className="flex flex-col items-center justify-center h-72 rounded-lg border border-neutral-950/10 bg-neutral-50 p-6 text-center dark:border-white/10 dark:bg-white/5">
      <p className="text-sm font-semibold text-neutral-900 dark:text-white">
        Static site
      </p>
      <p className="mt-1 text-sm text-neutral-600 dark:text-neutral-400">
        This service does not have running containers to manage.
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
  )
}

export function EnvironmentDashboardSkeleton() {
  return (
    <div
      role="status"
      className="min-w-0 space-y-6"
      aria-label="Loading environment"
    >
      <div className="flex items-center gap-3 border-b pb-2">
        {ENVIRONMENT_VIEWS.map((view) => (
          <Skeleton key={view.value} className="h-7 w-20" />
        ))}
      </div>
      <Skeleton className="h-24 w-full" />
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

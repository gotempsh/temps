import { getEnvironmentOptions } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { ContainerManagement } from '@/components/containers/ContainerManagement'
import { EnvironmentSettingsContent } from '@/components/environments/EnvironmentSettingsContent'
import { EnvironmentSidebar } from '@/components/environments/EnvironmentSidebar'
import { useQuery } from '@tanstack/react-query'
import { useSearchParams } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import { useCallback } from 'react'

interface EnvironmentDashboardProps {
  project: ProjectResponse
  environmentId: number
}

export function EnvironmentDashboard({
  project,
  environmentId,
}: EnvironmentDashboardProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const activeView = (searchParams.get('view') || 'containers') as string

  const handleViewChange = useCallback(
    (view: string) => {
      searchParams.set('view', view)
      setSearchParams(searchParams)
    },
    [searchParams, setSearchParams]
  )

  // Then, get the environment using the project ID
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

  const isLoading = isEnvironmentLoading
  const error = environmentError

  if (error) {
    return (
      <div className="p-6">
        <ErrorAlert
          title="Failed to load environment"
          description={
            error instanceof Error
              ? error.message
              : 'An unexpected error occurred'
          }
          retry={() => refetch()}
        />
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-6 border-b bg-background">
          <div className="flex items-center justify-between">
            <div>
              <Skeleton className="h-8 w-48 mb-2" />
              <Skeleton className="h-4 w-64" />
            </div>
            <div className="flex items-center gap-3">
              <Skeleton className="h-6 w-24" />
              <Skeleton className="h-9 w-24" />
            </div>
          </div>
        </div>
        <div className="flex-1 p-6">
          <Skeleton className="h-96 w-full" />
        </div>
      </div>
    )
  }

  if (!environment) {
    return (
      <div className="p-6">
        <ErrorAlert
          title="Environment not found"
          description="The environment you're looking for does not exist"
          retry={() => refetch()}
        />
      </div>
    )
  }

  // Check if the project is static (preset === 'custom' means it's a static site)
  const isStatic = project?.preset === 'custom'

  // Render content based on active view
  const renderContent = () => {
    switch (activeView) {
      case 'settings':
        return (
          <div className="space-y-6">
            <div>
              <h2 className="text-2xl font-bold">
                {environment.name} Settings
              </h2>
              <p className="text-sm text-muted-foreground mt-1">
                Configure domains, environment variables, and resources for this
                environment.
              </p>
            </div>
            <EnvironmentSettingsContent
              environment={environment}
              projectId={project?.id.toString() || ''}
              environmentId={environmentId.toString()}
            />
          </div>
        )
      case 'containers':
      default:
        return isStatic ? (
          <div className="flex flex-col items-center justify-center h-96 text-center">
            <p className="text-muted-foreground">
              This static site does not have running containers to manage.
            </p>
          </div>
        ) : (
          <ContainerManagement
            projectId={project?.id.toString() || ''}
            environmentId={environmentId.toString()}
          />
        )
    }
  }

  return (
    <div className="flex flex-col lg:flex-row h-full">
      {/* Sidebar/Navigation */}
      <EnvironmentSidebar
        environment={environment}
        activeView={activeView}
        onViewChange={handleViewChange}
        isStatic={isStatic}
      />

      {/* Main Content */}
      <div className="flex-1 overflow-auto flex flex-col">
        <div className="flex-1 p-6">{renderContent()}</div>
      </div>
    </div>
  )
}

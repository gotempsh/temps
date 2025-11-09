import { getEnvironmentOptions } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { ContainerManagement } from '@/components/containers/ContainerManagement'
import { EnvironmentSettingsDialog } from '@/components/environments/EnvironmentSettingsDialog'
import { useQuery } from '@tanstack/react-query'
import { useSearchParams } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'

interface EnvironmentDashboardProps {
  project: ProjectResponse
  environmentId: number
}

export function EnvironmentDashboard({
  project,
  environmentId,
}: EnvironmentDashboardProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const showSettings = searchParams.get('settings') === 'true'

  const handleSettingsOpen = (open: boolean) => {
    if (open) {
      searchParams.set('settings', 'true')
    } else {
      searchParams.delete('settings')
    }
    setSearchParams(searchParams)
  }

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

  return (
    <div className="flex flex-col h-full">
      {/* Main Content */}
      <div className="flex-1 overflow-auto">
        <div className="p-6">
          <ContainerManagement
            projectId={project?.id.toString() || ''}
            environmentId={environmentId.toString()}
          />
        </div>
      </div>

      {/* Settings Dialog */}
      <EnvironmentSettingsDialog
        open={showSettings}
        onOpenChange={handleSettingsOpen}
        environment={environment}
        projectId={project?.id.toString() || ''}
      />
    </div>
  )
}

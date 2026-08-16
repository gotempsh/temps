import {
  getEnvironmentsOptions,
  createEnvironmentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { Route, Routes, useSearchParams } from 'react-router'
import { EnvironmentDashboard } from './EnvironmentDashboard'
import { ContainerDetailPage } from './ContainerDetailPage'
import { ProjectResponse } from '@/api/client'
import { CreateEnvironmentDialog } from '@/components/project/settings/environments/CreateEnvironmentDialog'
import { Button } from '@/components/ui/button'
import { Plus } from 'lucide-react'
import { toast } from 'sonner'
import {
  resolveEnvironmentSelection,
  updateEnvironmentSearchParams,
} from '@/lib/environment-navigation'

export function EnvironmentsTabsView({
  project,
}: {
  project: ProjectResponse
}) {
  const [searchParams, setSearchParams] = useSearchParams()
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)

  const {
    data: environments,
    isLoading: isEnvironmentsLoading,
    refetch: refetchEnvironments,
  } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project?.id || 0,
      },
    }),
    enabled: !!project?.id,
  })

  const createEnv = useMutation({
    ...createEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment created successfully')
      refetchEnvironments()
      setIsCreateDialogOpen(false)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to create environment')
    },
  })

  const activeEnvId = resolveEnvironmentSelection(
    searchParams.get('environment'),
    environments ?? []
  )

  if (isEnvironmentsLoading) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-6 border-b bg-background">
          <Skeleton className="h-8 w-48 mb-2" />
          <Skeleton className="h-4 w-64" />
        </div>
        <div className="flex-1 p-6">
          <Skeleton className="h-96 w-full" />
        </div>
      </div>
    )
  }

  if (!environments || environments.length === 0) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-6 border-b bg-background">
          <div className="flex items-center justify-between gap-2">
            <div className="min-w-0">
              <h1 className="text-2xl sm:text-3xl font-bold">Environments</h1>
              <p className="text-sm text-muted-foreground mt-1">
                Manage and monitor your environments
              </p>
            </div>
            <Button onClick={() => setIsCreateDialogOpen(true)}>
              <Plus className="h-4 w-4 sm:mr-2" />
              <span className="hidden sm:inline">Add Environment</span>
            </Button>
          </div>
        </div>
        <div className="flex-1 flex items-center justify-center">
          <p className="text-muted-foreground">No environments found</p>
        </div>
        <CreateEnvironmentDialog
          open={isCreateDialogOpen}
          onOpenChange={setIsCreateDialogOpen}
          project={project}
          onSubmit={async (values) => {
            await createEnv.mutateAsync({
              path: { project_id: project.id || 0 },
              body: values,
            })
          }}
        />
      </div>
    )
  }

  if (!activeEnvId) return null

  const handleDelete = async (deletedId: number) => {
    const remaining = environments.filter((e) => e.id !== deletedId)
    await refetchEnvironments()
    const nextParams = updateEnvironmentSearchParams(searchParams, {
      environmentId: remaining[0]?.id ?? null,
    })
    setSearchParams(nextParams, { replace: true })
  }

  const handleEnvironmentChange = (id: number) => {
    const nextParams = updateEnvironmentSearchParams(searchParams, {
      environmentId: id,
    })
    setSearchParams(nextParams)
  }

  return (
    <div className="flex flex-col h-full">
      <Routes>
        <Route
          path="containers/:containerId"
          element={<ContainerDetailPage project={project} />}
        />
        <Route
          path="*"
          element={
            <EnvironmentDashboard
              key={activeEnvId}
              project={project}
              environmentId={activeEnvId}
              environments={environments}
              onEnvironmentChange={handleEnvironmentChange}
              onCreateEnvironment={() => setIsCreateDialogOpen(true)}
              onDelete={() => handleDelete(activeEnvId)}
            />
          }
        />
      </Routes>
      <CreateEnvironmentDialog
        open={isCreateDialogOpen}
        onOpenChange={setIsCreateDialogOpen}
        project={project}
        onSubmit={async (values) => {
          await createEnv.mutateAsync({
            path: { project_id: project.id || 0 },
            body: values,
          })
        }}
      />
    </div>
  )
}

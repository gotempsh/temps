import {
  getEnvironmentsOptions,
  createEnvironmentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery, useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { EnvironmentDashboard } from './EnvironmentDashboard'
import { ProjectResponse } from '@/api/client'
import { CreateEnvironmentDialog } from '@/components/project/settings/environments/CreateEnvironmentDialog'
import { toast } from 'sonner'

export function EnvironmentsTabsView({
  project,
}: {
  project: ProjectResponse
}) {
  const [selectedEnvId, setSelectedEnvId] = useState<number | undefined>(
    undefined
  )
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)

  // Then get environments
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

  // Create environment mutation
  const createEnv = useMutation({
    ...createEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment created successfully')
      refetchEnvironments()
      setIsCreateDialogOpen(false)
    },
    onError: (error: any) => {
      toast.error(error?.message || 'Failed to create environment')
    },
  })

  const isLoading = isEnvironmentsLoading

  // Use the selected environment or default to first one
  const activeEnvId = selectedEnvId ?? environments?.[0]?.id

  if (isLoading) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-6 border-b bg-background">
          <div>
            <Skeleton className="h-8 w-48 mb-2" />
            <Skeleton className="h-4 w-64" />
          </div>
        </div>
        <div className="flex-1 p-6">
          <Skeleton className="h-12 w-full mb-6" />
          <Skeleton className="h-96 w-full" />
        </div>
      </div>
    )
  }

  if (!environments || environments.length === 0) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-6 border-b bg-background">
          <div className="flex items-center justify-between">
            <div>
              <h1 className="text-3xl font-bold">Environments</h1>
              <p className="text-sm text-muted-foreground mt-1">
                Manage and monitor your environments
              </p>
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
        </div>
        <div className="flex-1 flex items-center justify-center">
          <p className="text-muted-foreground">No environments found</p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      <Tabs
        value={activeEnvId?.toString() || ''}
        onValueChange={(value) => setSelectedEnvId(parseInt(value))}
        className="flex flex-col h-full"
      >
        <div className="border-b bg-background sticky top-0 z-10">
          <div className="px-2 py-0">
            <div className="flex items-center justify-between mb-4">
              <h1 className="text-3xl font-bold">Environments</h1>
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
            <TabsList className="w-full justify-start overflow-x-auto h-auto p-0 bg-transparent border-b rounded-none">
              {environments.map((env) => (
                <TabsTrigger
                  key={env.id}
                  value={env.id.toString()}
                  className="rounded-none border-b-2 border-transparent data-[state=active]:border-primary px-4 py-3"
                >
                  <div className="flex flex-col items-start gap-1">
                    <span className="font-medium">{env.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {env.branch}
                    </span>
                  </div>
                </TabsTrigger>
              ))}
            </TabsList>
          </div>
        </div>

        <div className="flex-1 overflow-hidden">
          {environments.map((env) => (
            <TabsContent
              key={env.id}
              value={env.id.toString()}
              className="m-0 h-full overflow-auto"
            >
              <EnvironmentDashboard
                project={project}
                environmentId={env.id}
                onDelete={async () => {
                  // Find another environment to switch to
                  const remainingEnvs = environments.filter(
                    (e) => e.id !== env.id
                  )

                  // Refresh the environments list
                  await refetchEnvironments()

                  if (remainingEnvs.length > 0) {
                    // Switch to the first remaining environment
                    setSelectedEnvId(remainingEnvs[0].id)
                  } else {
                    // No more environments, clear the selection
                    setSelectedEnvId(undefined)
                  }
                }}
              />
            </TabsContent>
          ))}
        </div>
      </Tabs>
    </div>
  )
}

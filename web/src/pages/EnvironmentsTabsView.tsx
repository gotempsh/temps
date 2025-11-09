import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { EnvironmentDashboard } from './EnvironmentDashboard'
import { ProjectResponse } from '@/api/client'

export function EnvironmentsTabsView({
  project,
}: {
  project: ProjectResponse
}) {
  const [selectedEnvId, setSelectedEnvId] = useState<number | undefined>(
    undefined
  )

  // Then get environments
  const { data: environments, isLoading: isEnvironmentsLoading } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project?.id || 0,
      },
    }),
    enabled: !!project?.id,
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
          <h1 className="text-3xl font-bold">Environments</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Manage and monitor your environments
          </p>
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
            <h1 className="text-3xl font-bold mb-4">Environments</h1>
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
              <EnvironmentDashboard project={project} environmentId={env.id} />
            </TabsContent>
          ))}
        </div>
      </Tabs>
    </div>
  )
}

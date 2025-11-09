import { getContainerDetailOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery } from '@tanstack/react-query'
import { ContainerMetrics } from './ContainerMetrics'
import { ContainerLogs } from './ContainerLogs'
import { ContainerConfiguration } from './ContainerConfiguration'

interface ContainerDetailProps {
  projectId: string
  environmentId: string
  containerId: string
  tab: 'overview' | 'logs' | 'configuration'
  onTabChange: (tab: 'overview' | 'logs' | 'configuration') => void
  onAction: (action: 'start' | 'stop' | 'restart') => void
}

export function ContainerDetail({
  projectId,
  environmentId,
  containerId,
  tab,
  onTabChange,
  onAction,
}: ContainerDetailProps) {
  const { data: container, isLoading } = useQuery({
    ...getContainerDetailOptions({
      path: {
        project_id: parseInt(projectId || '0'),
        environment_id: parseInt(environmentId || '0'),
        container_id: containerId,
      },
    }),
  })

  if (isLoading) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="p-4 border-b bg-background">
          <Skeleton className="h-8 w-48 mb-2" />
          <Skeleton className="h-4 w-96" />
        </div>
        <div className="flex-1 p-4">
          <Skeleton className="h-96 w-full" />
        </div>
      </div>
    )
  }

  if (!container) {
    return (
      <div className="flex-1 flex items-center justify-center text-muted-foreground">
        Container not found
      </div>
    )
  }

  return (
    <div className="flex-1 flex flex-col">
      {/* Header */}
      <div className="p-6 border-b bg-background sticky top-0 z-10">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1">
            <h2 className="text-2xl font-bold">{container.container_name}</h2>
            <p className="text-sm text-muted-foreground mt-1">
              <code className="bg-muted px-2 py-0.5 rounded text-xs font-mono">
                {container.image_name}
              </code>
            </p>
          </div>
          <div className="flex items-center gap-2 flex-shrink-0">
            <Badge
              variant={container.status === 'running' ? 'default' : 'secondary'}
              className="text-sm"
            >
              {container.status}
            </Badge>
            {container.status === 'running' && (
              <>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => onAction('restart')}
                  className="text-xs"
                >
                  Restart
                </Button>
                <Button
                  size="sm"
                  variant="destructive"
                  onClick={() => onAction('stop')}
                  className="text-xs"
                >
                  Stop
                </Button>
              </>
            )}
            {container.status !== 'running' && (
              <Button
                size="sm"
                onClick={() => onAction('start')}
                className="text-xs"
              >
                Start
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* Tabs */}
      <Tabs
        value={tab}
        onValueChange={(value) =>
          onTabChange(value as 'overview' | 'logs' | 'configuration')
        }
        className="flex-1 flex flex-col"
      >
        <TabsList className="w-full justify-start rounded-none border-b bg-background/50">
          <TabsTrigger value="overview">Overview</TabsTrigger>
          <TabsTrigger value="logs">Logs</TabsTrigger>
          <TabsTrigger value="configuration">Configuration</TabsTrigger>
        </TabsList>

        <TabsContent value="overview" className="flex-1 overflow-auto">
          <div className="p-4 h-full">
            <ContainerMetrics
              projectId={projectId}
              environmentId={environmentId}
              containerId={containerId}
            />
          </div>
        </TabsContent>

        <TabsContent value="logs" className="flex-1 overflow-hidden">
          <ContainerLogs
            projectId={projectId}
            environmentId={environmentId}
            containerId={containerId}
          />
        </TabsContent>

        <TabsContent value="configuration" className="flex-1 overflow-auto">
          <div className="p-4">
            <ContainerConfiguration container={container} />
          </div>
        </TabsContent>
      </Tabs>
    </div>
  )
}

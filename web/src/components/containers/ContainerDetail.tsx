import { getContainerDetailOptions } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { ContainerLogs } from './ContainerLogs'
import { ContainerConfiguration } from './ContainerConfiguration'

interface ContainerDetailProps {
  projectId: string
  environmentId: string
  containerId: string
  tab: 'logs' | 'configuration'
}

export function ContainerDetail({
  projectId,
  environmentId,
  containerId,
  tab,
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
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  if (!container) {
    return (
      <div className="flex items-center justify-center text-muted-foreground py-24">
        Container not found
      </div>
    )
  }

  if (tab === 'configuration') {
    return <ContainerConfiguration container={container} />
  }

  return (
    <ContainerLogs
      projectId={projectId}
      environmentId={environmentId}
      containerId={containerId}
      serviceName={container.service_name ?? container.container_name ?? null}
    />
  )
}

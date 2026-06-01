import { ContainerLogsViewer } from './ContainerLogsViewer'

interface ContainerLogsProps {
  projectId: string
  environmentId: string
  containerId: string
  serviceName?: string | null
}

export function ContainerLogs({
  projectId,
  environmentId,
  containerId,
  serviceName,
}: ContainerLogsProps) {
  const fetchUrl = `/api/projects/${projectId}/environments/${environmentId}/containers/${containerId}/logs`

  return (
    <ContainerLogsViewer
      fetchUrl={fetchUrl}
      containerId={containerId}
      serviceName={serviceName}
    />
  )
}

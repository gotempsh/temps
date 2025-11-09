import { ContainerLogsViewer } from './ContainerLogsViewer'

interface ContainerLogsProps {
  projectId: string
  environmentId: string
  containerId: string
}

export function ContainerLogs({
  projectId,
  environmentId,
  containerId,
}: ContainerLogsProps) {
  const fetchUrl = `/api/projects/${projectId}/environments/${environmentId}/containers/${containerId}/logs`

  return <ContainerLogsViewer fetchUrl={fetchUrl} containerId={containerId} />
}

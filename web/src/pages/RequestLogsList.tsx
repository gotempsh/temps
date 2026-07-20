import { useNavigate } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import ProxyLogsList from '@/components/logs/ProxyLogsList'

interface RequestLogsListProps {
  project: ProjectResponse
}

export default function RequestLogsList({
  project: projectResponse,
}: RequestLogsListProps) {
  const navigate = useNavigate()

  const handleRowClick = (
    requestId: string,
    _projectId: number,
    timestamp: string
  ) => {
    // Navigate by request_id, not serial id: the ClickHouse backend has no
    // serial id column (list rows surface id=0 there), while request_id
    // resolves under both backends. The row's timestamp lets the detail
    // endpoint bound its lookup to the right chunks/partitions instead of
    // scanning the whole retention window.
    navigate(
      `/projects/${projectResponse.slug}/logs/${encodeURIComponent(requestId)}?ts=${encodeURIComponent(timestamp)}`
    )
  }

  return (
    <div className="w-full py-4 sm:py-6">
      <ProxyLogsList project={projectResponse} onRowClick={handleRowClick} />
    </div>
  )
}

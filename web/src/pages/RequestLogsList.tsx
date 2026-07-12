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
    logId: number,
    _projectId: number,
    timestamp: string
  ) => {
    // The row's timestamp lets the detail endpoint bound its hypertable
    // lookup to the right chunks instead of scanning the whole retention
    // window.
    navigate(
      `/projects/${projectResponse.slug}/logs/${logId}?ts=${encodeURIComponent(timestamp)}`
    )
  }

  return (
    <div className="w-full py-4 sm:py-6">
      <ProxyLogsList project={projectResponse} onRowClick={handleRowClick} />
    </div>
  )
}

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

  const handleRowClick = (logId: number) => {
    navigate(`/projects/${projectResponse.slug}/logs/${logId}`)
  }

  return (
    <div className="container mx-auto py-6">
      <ProxyLogsList project={projectResponse} onRowClick={handleRowClick} />
    </div>
  )
}

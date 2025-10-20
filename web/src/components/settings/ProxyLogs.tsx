import { useNavigate } from 'react-router-dom'
import { ProxyLogsDataTable } from '@/components/proxy-logs/ProxyLogsDataTable'
import { ProxyLogResponse } from '@/api/client/types.gen'

export default function ProxyLogs() {
  const navigate = useNavigate()

  const handleRowClick = (log: ProxyLogResponse) => {
    // Navigate to the proxy log detail page
    navigate(`/proxy-logs/${log.id}`)
  }

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-medium">Proxy Logs</h3>
        <p className="text-sm text-muted-foreground">
          Advanced proxy request logs with comprehensive filtering across all
          projects
        </p>
      </div>
      <ProxyLogsDataTable onRowClick={handleRowClick} />
    </div>
  )
}

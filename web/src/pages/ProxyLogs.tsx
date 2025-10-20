import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { ProxyLogsDataTable } from '@/components/proxy-logs/ProxyLogsDataTable'
import { ProxyLogResponse } from '@/api/client/types.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function ProxyLogs() {
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy Logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy Logs')

  const handleRowClick = (log: ProxyLogResponse) => {
    // Navigate to the proxy log detail page
    navigate(`/proxy-logs/${log.id}`)
  }

  return (
    <div className="container max-w-7xl mx-auto py-8">
      <div className="space-y-6">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Proxy Logs</h2>
          <p className="text-muted-foreground">
            Advanced proxy request logs with comprehensive filtering and sorting
          </p>
        </div>
        <ProxyLogsDataTable onRowClick={handleRowClick} />
      </div>
    </div>
  )
}

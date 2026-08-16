import { useEffect } from 'react'
import { ProxyLogsDataTable } from '@/components/proxy-logs/ProxyLogsDataTable'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function ProxyLogs() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Proxy Logs' }])
  }, [setBreadcrumbs])

  usePageTitle('Proxy Logs')

  return (
    <div className="w-full py-8">
      <div className="space-y-6">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Proxy Logs</h2>
          <p className="text-muted-foreground">
            Advanced proxy request logs with comprehensive filtering and sorting
          </p>
        </div>
        <ProxyLogsDataTable />
      </div>
    </div>
  )
}

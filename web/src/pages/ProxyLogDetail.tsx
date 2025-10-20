import { ProxyLogDetail } from '@/components/proxy-logs/ProxyLogDetail'
import { Button } from '@/components/ui/button'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { ArrowLeft } from 'lucide-react'
import { useEffect } from 'react'
import { useNavigate, useParams } from 'react-router-dom'

export default function ProxyLogDetailPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const logId = parseInt(id || '0', 10)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Proxy Logs', href: '/proxy-logs' },
      { label: `Log #${logId}` },
    ])
  }, [setBreadcrumbs, logId])

  usePageTitle(`Proxy Log #${logId}`)

  if (!id || isNaN(logId)) {
    return (
      <div className="container max-w-7xl mx-auto py-8">
        <div className="text-center">
          <h2 className="text-2xl font-bold">Invalid Log ID</h2>
          <p className="text-muted-foreground mt-2">
            Please provide a valid log ID
          </p>
          <Button onClick={() => navigate('/proxy-logs')} className="mt-4">
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Proxy Logs
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="container max-w-7xl mx-auto py-8">
      <div className="space-y-6">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-4">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/proxy-logs')}
            >
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back
            </Button>
            <div>
              <h2 className="text-2xl font-bold tracking-tight">
                Proxy Log Details
              </h2>
              <p className="text-muted-foreground">
                Detailed information about proxy request #{logId}
              </p>
            </div>
          </div>
        </div>

        {/* Detail Component */}
        <ProxyLogDetail logId={logId} />
      </div>
    </div>
  )
}

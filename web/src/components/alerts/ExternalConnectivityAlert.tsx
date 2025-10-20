import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useExternalConnectivity } from '@/hooks/useExternalConnectivity'
import { Globe, ExternalLink, AlertCircle, X } from 'lucide-react'
import { Link } from 'react-router-dom'
import { useState } from 'react'

interface ExternalConnectivityAlertProps {
  showInDashboard?: boolean
  onDismiss?: () => void
  dismissible?: boolean
}

export function ExternalConnectivityAlert({
  showInDashboard = false,
  onDismiss,
  dismissible = false,
}: ExternalConnectivityAlertProps) {
  const { isConfigured, missingConfigs } = useExternalConnectivity()
  const [isDismissed, setIsDismissed] = useState(false)

  // Don't show if configured or dismissed
  if (isConfigured || isDismissed) {
    return null
  }

  const handleDismiss = () => {
    setIsDismissed(true)
    onDismiss?.()
  }

  if (showInDashboard) {
    return (
      <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20 mb-6">
        <div className="flex items-start justify-between w-full">
          <div className="flex items-start gap-3 flex-1">
            <Globe className="h-5 w-5 text-orange-600 mt-0.5" />
            <div className="space-y-1 flex-1">
              <AlertTitle className="text-orange-900 dark:text-orange-100">
                External Connectivity Setup Required
              </AlertTitle>
              <AlertDescription className="text-orange-800 dark:text-orange-200">
                Your platform needs external connectivity configuration to be
                accessible from the internet.
                {missingConfigs.length > 0 && (
                  <span className="block mt-1">
                    Missing: <strong>{missingConfigs.join(', ')}</strong>
                  </span>
                )}
              </AlertDescription>
            </div>
          </div>
          <div className="flex items-center gap-2 ml-4">
            <Link to="/setup/connectivity">
              <Button
                size="sm"
                variant="outline"
                className="border-orange-300 text-orange-700 hover:bg-orange-100 dark:border-orange-700 dark:text-orange-300 dark:hover:bg-orange-900/20"
              >
                <ExternalLink className="h-4 w-4 mr-1" />
                Configure Now
              </Button>
            </Link>
            {dismissible && (
              <Button
                size="sm"
                variant="ghost"
                onClick={handleDismiss}
                className="h-8 w-8 p-0 text-orange-600 hover:bg-orange-100 dark:text-orange-400 dark:hover:bg-orange-900/20"
              >
                <X className="h-4 w-4" />
              </Button>
            )}
          </div>
        </div>
      </Alert>
    )
  }

  return (
    <Alert variant="destructive" className="mb-4">
      <AlertCircle className="h-4 w-4" />
      <div className="flex items-center justify-between w-full">
        <div>
          <AlertTitle>External Connectivity Not Configured</AlertTitle>
          <AlertDescription>
            Your platform cannot be accessed from the internet. Configure
            external connectivity to enable domain access.
          </AlertDescription>
        </div>
        <Link to="/setup/connectivity">
          <Button variant="outline" size="sm">
            <Globe className="h-4 w-4 mr-2" />
            Setup Now
          </Button>
        </Link>
      </div>
    </Alert>
  )
}

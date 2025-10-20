import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  usePlatformAccess,
  useAccessMode,
  useIsLocalMode,
  useIsNatMode,
  useIsCloudflareMode,
  useIsDirectMode,
} from '@/hooks/usePlatformAccess'
import {
  AlertCircle,
  CheckCircle2,
  Globe,
  Network,
  Server,
  Shield,
} from 'lucide-react'

/**
 * Example component showing how to use the Platform Access Context
 * This demonstrates different ways to access platform access information
 */
export function PlatformAccessExample() {
  // Full context with loading states and error handling
  const { accessInfo, isLoading, error, refetch } = usePlatformAccess()

  // Helper hooks for specific access modes
  const accessMode = useAccessMode()
  const isLocal = useIsLocalMode()
  const isNat = useIsNatMode()
  const isCloudflare = useIsCloudflareMode()
  const isDirect = useIsDirectMode()

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Platform Access Information</CardTitle>
          <CardDescription>Loading platform access details...</CardDescription>
        </CardHeader>
      </Card>
    )
  }

  if (error) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <AlertCircle className="h-5 w-5 text-destructive" />
            Platform Access Error
          </CardTitle>
          <CardDescription>
            Failed to load platform access information: {error.message}
            <button
              onClick={() => refetch()}
              className="ml-2 text-primary hover:underline"
            >
              Retry
            </button>
          </CardDescription>
        </CardHeader>
      </Card>
    )
  }

  const getAccessModeIcon = () => {
    switch (accessMode) {
      case 'local':
        return <Server className="h-4 w-4" />
      case 'direct':
        return <Globe className="h-4 w-4" />
      case 'nat':
        return <Network className="h-4 w-4" />
      case 'cloudflare_tunnel':
        return <Shield className="h-4 w-4" />
      default:
        return <AlertCircle className="h-4 w-4" />
    }
  }

  const getAccessModeColor = () => {
    switch (accessMode) {
      case 'local':
        return 'bg-blue-50 text-blue-700 border-blue-200'
      case 'direct':
        return 'bg-green-50 text-green-700 border-green-200'
      case 'nat':
        return 'bg-yellow-50 text-yellow-700 border-yellow-200'
      case 'cloudflare_tunnel':
        return 'bg-purple-50 text-purple-700 border-purple-200'
      default:
        return 'bg-gray-50 text-gray-700 border-gray-200'
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <CheckCircle2 className="h-5 w-5 text-green-500" />
          Platform Access Information
        </CardTitle>
        <CardDescription>
          Current platform access mode and network configuration
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium">Access Mode:</span>
          <Badge className={getAccessModeColor()}>
            {getAccessModeIcon()}
            <span className="ml-1 capitalize">{accessMode}</span>
          </Badge>
        </div>

        {accessInfo?.public_ip && (
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">Public IP:</span>
            <code className="text-xs bg-muted px-2 py-1 rounded">
              {accessInfo.public_ip}
            </code>
          </div>
        )}

        {accessInfo?.private_ip && (
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">Private IP:</span>
            <code className="text-xs bg-muted px-2 py-1 rounded">
              {accessInfo.private_ip}
            </code>
          </div>
        )}

        <div className="mt-4 p-3 bg-muted/50 rounded-lg">
          <h4 className="text-sm font-medium mb-2">Helper Hook Results:</h4>
          <div className="space-y-1 text-xs">
            <div>
              isLocal:{' '}
              <Badge variant={isLocal ? 'default' : 'secondary'}>
                {isLocal.toString()}
              </Badge>
            </div>
            <div>
              isDirect:{' '}
              <Badge variant={isDirect ? 'default' : 'secondary'}>
                {isDirect.toString()}
              </Badge>
            </div>
            <div>
              isNat:{' '}
              <Badge variant={isNat ? 'default' : 'secondary'}>
                {isNat.toString()}
              </Badge>
            </div>
            <div>
              isCloudflare:{' '}
              <Badge variant={isCloudflare ? 'default' : 'secondary'}>
                {isCloudflare.toString()}
              </Badge>
            </div>
          </div>
        </div>

        <div className="mt-4 p-3 bg-blue-50/50 dark:bg-blue-950/20 rounded-lg">
          <h4 className="text-sm font-medium mb-2">Usage Example:</h4>
          <pre className="text-xs bg-background p-2 rounded border overflow-x-auto">
            {`// Import the hooks you need
import { useAccessMode, useIsLocalMode } from '@/contexts/PlatformAccessContext'

// In your component
const accessMode = useAccessMode()
const isLocal = useIsLocalMode()

// Conditional logic based on access mode
if (isLocal) {
  // Show localhost-specific UI
} else if (accessMode === 'cloudflare_tunnel') {
  // Show Cloudflare-specific features
}`}
          </pre>
        </div>
      </CardContent>
    </Card>
  )
}

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useSettings } from '@/hooks/useSettings'
import { usePlatformConfigured } from '@/hooks/usePlatformConfig'
import {
  CheckCircle2,
  XCircle,
  AlertCircle,
  Settings,
  ExternalLink,
} from 'lucide-react'
import { Link } from 'react-router-dom'

/**
 * Component to display the current platform configuration status
 * Can be used in dashboards or admin panels to show configuration health
 */
export function PlatformConfigStatus() {
  const { data: settings, isLoading } = useSettings()
  const { isConfigured, missingConfigs } = usePlatformConfigured()

  if (isLoading) {
    return (
      <Card>
        <CardContent className="p-6">
          <div className="animate-pulse space-y-3">
            <div className="h-4 bg-muted rounded w-1/4"></div>
            <div className="h-3 bg-muted rounded w-3/4"></div>
          </div>
        </CardContent>
      </Card>
    )
  }

  const configItems = [
    {
      label: 'DNS Provider',
      value:
        settings?.dns_provider.provider === 'cloudflare'
          ? 'Cloudflare'
          : 'Manual',
      configured: true,
      icon:
        settings?.dns_provider.provider === 'cloudflare' ? (
          <CheckCircle2 className="h-4 w-4 text-green-500" />
        ) : (
          <AlertCircle className="h-4 w-4 text-yellow-500" />
        ),
    },
    {
      label: 'External URL',
      value: settings?.external_url || 'Not configured',
      configured: !!settings?.external_url,
      icon: settings?.external_url ? (
        <CheckCircle2 className="h-4 w-4 text-green-500" />
      ) : (
        <XCircle className="h-4 w-4 text-red-500" />
      ),
    },
    {
      label: "Let's Encrypt",
      value: settings?.letsencrypt.email || 'No email set',
      configured: !!settings?.letsencrypt.email,
      icon: settings?.letsencrypt.email ? (
        <CheckCircle2 className="h-4 w-4 text-green-500" />
      ) : (
        <XCircle className="h-4 w-4 text-red-500" />
      ),
    },
    {
      label: 'Preview Domain',
      value: settings?.preview_domain || 'Not set',
      configured: !!settings?.preview_domain,
      icon: <CheckCircle2 className="h-4 w-4 text-green-500" />,
    },
    {
      label: 'Screenshots',
      value: settings?.screenshots.enabled
        ? `Enabled (${settings.screenshots.provider})`
        : 'Disabled',
      configured: true,
      icon: settings?.screenshots.enabled ? (
        <CheckCircle2 className="h-4 w-4 text-green-500" />
      ) : (
        <AlertCircle className="h-4 w-4 text-gray-400" />
      ),
    },
  ]

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              <Settings className="h-5 w-5" />
              Platform Configuration
            </CardTitle>
            <CardDescription>
              Current platform settings and configuration status
            </CardDescription>
          </div>
          <Link to="/settings">
            <Button variant="outline" size="sm">
              <Settings className="h-4 w-4 mr-2" />
              Manage Settings
            </Button>
          </Link>
        </div>
      </CardHeader>
      <CardContent>
        {!isConfigured && missingConfigs.length > 0 && (
          <Alert variant="destructive" className="mb-4">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Configuration Required</AlertTitle>
            <AlertDescription>
              The following settings need to be configured:{' '}
              {missingConfigs.join(', ')}
            </AlertDescription>
          </Alert>
        )}

        <div className="space-y-3">
          {configItems.map((item) => (
            <div
              key={item.label}
              className="flex items-center justify-between py-2 border-b last:border-0"
            >
              <div className="flex items-center gap-2">
                {item.icon}
                <span className="text-sm font-medium">{item.label}</span>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-sm text-muted-foreground">
                  {item.value}
                </span>
                {item.label === 'External URL' && settings?.external_url && (
                  <a
                    href={settings.external_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-blue-500 hover:text-blue-600"
                  >
                    <ExternalLink className="h-3 w-3" />
                  </a>
                )}
              </div>
            </div>
          ))}
        </div>

        {settings?.dns_provider.provider === 'cloudflare' &&
          settings.dns_provider.cloudflare_api_key && (
            <div className="mt-4 p-3 bg-blue-50 dark:bg-blue-950 rounded-lg">
              <div className="flex items-center gap-2">
                <CheckCircle2 className="h-4 w-4 text-blue-500" />
                <span className="text-sm font-medium">
                  Cloudflare Integration Active
                </span>
              </div>
              <p className="text-xs text-muted-foreground mt-1">
                DNS records will be automatically managed through Cloudflare API
              </p>
            </div>
          )}

        {settings?.screenshots.enabled && (
          <div className="mt-4 p-3 bg-green-50 dark:bg-green-950 rounded-lg">
            <div className="flex items-center gap-2">
              <CheckCircle2 className="h-4 w-4 text-green-500" />
              <span className="text-sm font-medium">
                Screenshot Generation Enabled
              </span>
            </div>
            <p className="text-xs text-muted-foreground mt-1">
              Using{' '}
              {settings.screenshots.provider === 'external'
                ? 'external API'
                : 'local service'}{' '}
              for screenshots
            </p>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

/**
 * Compact badge component showing configuration status
 */
export function PlatformConfigBadge() {
  const { isConfigured } = usePlatformConfigured()

  return (
    <Badge variant={isConfigured ? 'default' : 'destructive'}>
      {isConfigured ? 'Configured' : 'Setup Required'}
    </Badge>
  )
}

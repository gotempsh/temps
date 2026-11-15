// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { usePlatformCapabilities } from '@/hooks/usePlatformCapabilities'
import { useSettings } from '@/hooks/useSettings'
import {
  AlertCircle,
  CheckCircle2,
  Cloud,
  Globe,
  Info,
  Network,
  Router,
  Server,
} from 'lucide-react'

/**
 * Component that displays DNS configuration instructions based on the platform access mode
 * Shows the correct IP address to use for DNS records and mode-specific guidance.
 * It always renders something: a configured platform still adds new domains,
 * and the disclosure around it would otherwise open onto nothing.
 */
export function DNSConfigurationHelper() {
  const {
    accessMode,
    getDNSTargetIP,
    isUsingCloudflare,
    needsPortForwarding,
    isLoading,
    publicIP,
  } = usePlatformCapabilities()

  const { data: settings } = useSettings()

  const targetIP = getDNSTargetIP()

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>DNS configuration</CardTitle>
          <CardDescription>
            Loading platform access information...
          </CardDescription>
        </CardHeader>
      </Card>
    )
  }

  // Cloudflare Tunnel - domains managed externally
  if (isUsingCloudflare()) {
    return (
      <Alert className="border-purple-200 dark:border-purple-900 bg-purple-50/50 dark:bg-purple-950/10">
        <Cloud className="h-4 w-4 text-purple-600" />
        <AlertTitle>Cloudflare Tunnel Active</AlertTitle>
        <AlertDescription className="space-y-2 mt-2">
          <p>
            Domains and SSL certificates are automatically managed by
            Cloudflare.
          </p>
          <div className="flex items-center gap-2 mt-3">
            <Badge
              variant="secondary"
              className="bg-purple-100 dark:bg-purple-950/40 text-purple-800 dark:text-purple-400"
            >
              Automatic SSL
            </Badge>
            <Badge
              variant="secondary"
              className="bg-purple-100 dark:bg-purple-950/40 text-purple-800 dark:text-purple-400"
            >
              Managed DNS
            </Badge>
            <Badge
              variant="secondary"
              className="bg-purple-100 dark:bg-purple-950/40 text-purple-800 dark:text-purple-400"
            >
              DDoS protection
            </Badge>
          </div>
          <p className="text-sm mt-3">
            To add or remove domains, configure them through your{' '}
            <a
              href="https://dash.cloudflare.com"
              target="_blank"
              rel="noopener noreferrer"
              className="text-purple-600 dark:text-purple-400 hover:text-purple-700 dark:hover:text-purple-300 underline"
            >
              Cloudflare Dashboard
            </a>
          </p>
        </AlertDescription>
      </Alert>
    )
  }

  // No public IP available
  if (!targetIP && accessMode !== 'local') {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>DNS Configuration Unavailable</AlertTitle>
        <AlertDescription>
          Unable to determine public IP address. Please check your external
          connectivity setup.
        </AlertDescription>
      </Alert>
    )
  }

  // The access mode describes how this browser reached the server, not how the
  // server is exposed: a platform with a public URL can still be opened locally.
  if (accessMode === 'local' && settings?.external_url) {
    return (
      <Alert>
        <Info className="h-4 w-4" />
        <AlertTitle>
          Point DNS at the same address as your public URL
        </AlertTitle>
        <AlertDescription className="space-y-3 mt-2">
          <p>
            This console is open through a local address, so it can&apos;t tell
            how the internet reaches this server. Point your domain at the same
            address as{' '}
            <code className="font-mono">{settings.external_url}</code>.
          </p>
          {publicIP && (
            <div className="flex flex-wrap items-center gap-2">
              <span>Detected public IP</span>
              <code className="rounded border bg-background px-2 py-0.5 font-mono">
                {publicIP}
              </code>
              <CopyButton
                value={publicIP}
                className="h-8 px-3 rounded-md border border-input bg-background hover:bg-accent hover:text-accent-foreground"
              />
            </div>
          )}
        </AlertDescription>
      </Alert>
    )
  }

  // Local mode - needs external setup
  if (accessMode === 'local') {
    return (
      <Alert className="border-yellow-200 dark:border-yellow-900 bg-yellow-50/50 dark:bg-yellow-950/10">
        <Server className="h-4 w-4 text-yellow-600" />
        <AlertTitle>Local Development Mode</AlertTitle>
        <AlertDescription className="space-y-2 mt-2">
          <p>Your platform is running in local mode without external access.</p>
          <p className="text-sm">
            To enable domain and certificate management, configure external
            access through:
          </p>
          <ul className="text-sm list-disc list-inside mt-2">
            <li>Port forwarding from your router</li>
            <li>Cloudflare Tunnel for secure access</li>
            <li>VPS deployment with public IP</li>
          </ul>
        </AlertDescription>
      </Alert>
    )
  }

  // Direct or NAT mode with public IP
  if (targetIP) {
    const getAccessModeIcon = () => {
      switch (accessMode) {
        case 'direct':
          return <Globe className="h-5 w-5 text-green-600" />
        case 'nat':
          return <Router className="h-5 w-5 text-orange-600" />
        default:
          return <Network className="h-5 w-5 text-blue-600" />
      }
    }

    const getAccessModeBadgeColor = () => {
      switch (accessMode) {
        case 'direct':
          return 'bg-green-100 dark:bg-green-950/40 text-green-800 dark:text-green-400 border-green-200 dark:border-green-900'
        case 'nat':
          return 'bg-orange-100 dark:bg-orange-950/40 text-orange-800 dark:text-orange-400 border-orange-200 dark:border-orange-900'
        default:
          return 'bg-blue-100 dark:bg-blue-950/40 text-blue-800 dark:text-blue-400 border-blue-200 dark:border-blue-900'
      }
    }

    return (
      <Card>
        <CardHeader>
          <div className="flex items-start justify-between gap-2">
            <div className="space-y-1">
              <CardTitle className="flex items-center gap-2">
                {getAccessModeIcon()}
                DNS configuration
              </CardTitle>
              <CardDescription>
                Configure your DNS records to point to this server
              </CardDescription>
            </div>
            <Badge variant="outline" className={getAccessModeBadgeColor()}>
              {accessMode === 'direct' ? 'Direct Access' : 'NAT Mode'}
            </Badge>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* Public IP Display */}
          <div className="p-4 bg-muted/50 rounded-lg space-y-3">
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
              <span className="text-sm font-medium">DNS Target IP Address</span>
              <div className="flex items-center gap-2">
                <code className="px-3 py-1 bg-background border rounded text-lg font-mono font-semibold">
                  {targetIP}
                </code>
                <CopyButton
                  value={targetIP}
                  className="h-8 px-3 rounded-md border border-input bg-background hover:bg-accent hover:text-accent-foreground"
                />
              </div>
            </div>

            {needsPortForwarding() && (
              <Alert className="border-orange-200 dark:border-orange-900 bg-orange-50/50 dark:bg-orange-950/10">
                <Router className="h-4 w-4 text-orange-600" />
                <AlertDescription>
                  <strong>Port Forwarding Required:</strong> Ensure ports 80
                  (HTTP) and 443 (HTTPS) are forwarded from your router to this
                  server&apos;s internal IP.
                </AlertDescription>
              </Alert>
            )}
          </div>

          {/* DNS Record Examples */}
          <div className="space-y-3">
            <h4 className="text-sm font-medium">Example DNS records</h4>
            <div className="space-y-2">
              <div className="p-3 bg-muted/30 rounded-md border">
                <div className="flex items-center justify-between">
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">
                      Wildcard subdomain (for all services)
                    </div>
                    <code className="text-sm font-mono">
                      A *.yourdomain.com {targetIP}
                    </code>
                  </div>
                  <CopyButton
                    value={`A    *.yourdomain.com    ${targetIP}`}
                    className="h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
                  />
                </div>
              </div>

              <div className="p-3 bg-muted/30 rounded-md border">
                <div className="flex items-center justify-between">
                  <div>
                    <div className="text-xs text-muted-foreground mb-1">
                      Root domain (optional)
                    </div>
                    <code className="text-sm font-mono">
                      A yourdomain.com {targetIP}
                    </code>
                  </div>
                  <CopyButton
                    value={`A    yourdomain.com      ${targetIP}`}
                    className="h-8 w-8 p-0 hover:bg-accent hover:text-accent-foreground rounded-md"
                  />
                </div>
              </div>
            </div>
          </div>

          {/* TTL Recommendation */}
          <div className="p-3 bg-blue-50/50 dark:bg-blue-950/10 rounded-lg border border-blue-200 dark:border-blue-900">
            <div className="flex gap-2">
              <Info className="h-4 w-4 text-blue-600 mt-0.5" />
              <div className="space-y-1 text-sm">
                <p className="font-medium text-blue-900 dark:text-blue-100">
                  DNS Configuration Tips:
                </p>
                <ul className="text-blue-800 dark:text-blue-200 list-disc list-inside space-y-0.5">
                  <li>
                    Set TTL to 300 seconds (5 minutes) for faster propagation
                    during setup
                  </li>
                  <li>
                    Once stable, increase TTL to 3600 seconds (1 hour) or higher
                  </li>
                  <li>
                    DNS changes can take up to 48 hours to propagate globally
                  </li>
                  {needsPortForwarding() && (
                    <li className="text-orange-700 dark:text-orange-300 font-medium">
                      Remember to configure port forwarding before testing
                      domains
                    </li>
                  )}
                </ul>
              </div>
            </div>
          </div>

          {/* SSL Certificate Note */}
          <div className="flex items-start gap-2 p-3 bg-green-50/50 dark:bg-green-950/10 rounded-lg border border-green-200 dark:border-green-900">
            <CheckCircle2 className="h-4 w-4 text-green-600 mt-0.5" />
            <div className="text-sm">
              <p className="text-green-900 dark:text-green-100">
                SSL certificates will be automatically provisioned via
                Let&apos;s Encrypt once DNS is configured.
              </p>
            </div>
          </div>
        </CardContent>
      </Card>
    )
  }

  return null
}

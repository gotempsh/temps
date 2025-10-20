import { getProxyLogByIdOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { Link as RouterLink } from 'react-router-dom'
import {
  Activity,
  AlertCircle,
  Bot,
  Clock,
  Code,
  FileText,
  Globe,
  HardDrive,
  Laptop,
  Link,
  MapPin,
  Monitor,
  Network,
  Server,
  Smartphone,
  Tablet,
  Zap,
} from 'lucide-react'

interface ProxyLogDetailProps {
  logId: number
}

function formatBytes(bytes: number | null | undefined): string {
  if (!bytes) return '-'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(2)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`
}

function getDeviceIcon(deviceType: string | null | undefined) {
  switch (deviceType?.toLowerCase()) {
    case 'mobile':
      return <Smartphone className="h-4 w-4" />
    case 'tablet':
      return <Tablet className="h-4 w-4" />
    case 'desktop':
      return <Monitor className="h-4 w-4" />
    default:
      return <Laptop className="h-4 w-4" />
  }
}

export function ProxyLogDetail({ logId }: ProxyLogDetailProps) {
  const {
    data: log,
    isLoading,
    error,
  } = useQuery({
    ...getProxyLogByIdOptions({
      path: { id: logId },
    }),
  })

  if (isLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-32 w-full" />
        <Skeleton className="h-64 w-full" />
        <Skeleton className="h-48 w-full" />
      </div>
    )
  }

  if (error || !log) {
    return (
      <Card>
        <CardContent className="flex flex-col items-center justify-center py-12">
          <AlertCircle className="h-12 w-12 text-destructive mb-4" />
          <h3 className="text-lg font-semibold">Proxy log not found</h3>
          <p className="text-sm text-muted-foreground">
            The proxy log you&apos;re looking for doesn&apos;t exist or has been
            deleted.
          </p>
        </CardContent>
      </Card>
    )
  }

  const getStatusBadgeVariant = (statusCode: number) => {
    if (statusCode >= 200 && statusCode < 300) return 'default'
    if (statusCode >= 300 && statusCode < 400) return 'secondary'
    if (statusCode >= 400 && statusCode < 500) return 'destructive'
    if (statusCode >= 500) return 'destructive'
    return 'outline'
  }

  const getRoutingStatusBadge = (status: string) => {
    switch (status) {
      case 'routed':
        return <Badge variant="default">Routed</Badge>
      case 'failed':
        return <Badge variant="destructive">Failed</Badge>
      case 'not_found':
        return <Badge variant="secondary">Not Found</Badge>
      default:
        return <Badge variant="outline">{status}</Badge>
    }
  }

  return (
    <div className="space-y-6">
      {/* Overview Card */}
      <Card>
        <CardHeader>
          <div className="flex items-start justify-between">
            <div>
              <CardTitle className="flex items-center gap-2">
                <Network className="h-5 w-5" />
                Request Overview
              </CardTitle>
              <CardDescription className="mt-2 font-mono text-xs">
                Request ID: {log.request_id}
              </CardDescription>
            </div>
            <div className="flex gap-2">
              <Badge variant={getStatusBadgeVariant(log.status_code)}>
                {log.status_code}
              </Badge>
              {getRoutingStatusBadge(log.routing_status)}
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <div className="flex items-center gap-3">
              <Clock className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Timestamp</p>
                <p className="text-sm text-muted-foreground">
                  {format(new Date(log.timestamp), 'PPpp')}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-3">
              <Zap className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Response Time</p>
                <p className="text-sm text-muted-foreground">
                  {log.response_time_ms ? `${log.response_time_ms}ms` : '-'}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-3">
              <Code className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Method</p>
                <p className="text-sm text-muted-foreground">{log.method}</p>
              </div>
            </div>
            <div className="flex items-center gap-3">
              <Activity className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm font-medium">Request Source</p>
                <Badge variant="secondary" className="capitalize">
                  {log.request_source}
                </Badge>
              </div>
            </div>
          </div>

          <Separator />

          <div>
            <div className="flex items-center gap-2 mb-2">
              <Globe className="h-4 w-4 text-muted-foreground" />
              <p className="text-sm font-medium">URL</p>
            </div>
            <div className="bg-muted rounded-md p-3 font-mono text-xs break-all">
              {log.host}
              {log.path}
              {log.query_string && (
                <span className="text-muted-foreground">
                  ?{log.query_string}
                </span>
              )}
            </div>
          </div>

          {log.referrer && (
            <div>
              <div className="flex items-center gap-2 mb-2">
                <Link className="h-4 w-4 text-muted-foreground" />
                <p className="text-sm font-medium">Referrer</p>
              </div>
              <div className="bg-muted rounded-md p-3 font-mono text-xs break-all">
                {log.referrer}
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Routing Information */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Server className="h-5 w-5" />
              Routing Information
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            {log.project_id && (
              <div>
                <p className="text-sm font-medium">Project ID</p>
                <p className="text-sm text-muted-foreground">
                  {log.project_id}
                </p>
              </div>
            )}
            {log.environment_id && (
              <div>
                <p className="text-sm font-medium">Environment ID</p>
                <p className="text-sm text-muted-foreground">
                  {log.environment_id}
                </p>
              </div>
            )}
            {log.deployment_id && (
              <div>
                <p className="text-sm font-medium">Deployment ID</p>
                <p className="text-sm text-muted-foreground">
                  {log.deployment_id}
                </p>
              </div>
            )}
            {log.container_id && (
              <div>
                <p className="text-sm font-medium">Container ID</p>
                <p className="text-sm text-muted-foreground font-mono text-xs">
                  {log.container_id}
                </p>
              </div>
            )}
            {log.upstream_host && (
              <div>
                <p className="text-sm font-medium">Upstream Host</p>
                <p className="text-sm text-muted-foreground font-mono text-xs">
                  {log.upstream_host}
                </p>
              </div>
            )}
            {log.cache_status && (
              <div>
                <p className="text-sm font-medium">Cache Status</p>
                <Badge variant="outline">{log.cache_status}</Badge>
              </div>
            )}
            {log.is_system_request && (
              <div>
                <Badge variant="secondary">System Request</Badge>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <HardDrive className="h-5 w-5" />
              Size & Performance
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid grid-cols-2 gap-4">
              <div>
                <p className="text-sm font-medium">Request Size</p>
                <p className="text-sm text-muted-foreground">
                  {formatBytes(log.request_size_bytes)}
                </p>
              </div>
              <div>
                <p className="text-sm font-medium">Response Size</p>
                <p className="text-sm text-muted-foreground">
                  {formatBytes(log.response_size_bytes)}
                </p>
              </div>
            </div>
            <Separator />
            <div>
              <p className="text-sm font-medium mb-2">Response Time</p>
              <div className="flex items-center gap-2">
                <div className="flex-1 bg-muted rounded-full h-2">
                  <div
                    className="bg-primary rounded-full h-2"
                    style={{
                      width: `${Math.min((log.response_time_ms || 0) / 10, 100)}%`,
                    }}
                  />
                </div>
                <span className="text-sm font-mono">
                  {log.response_time_ms ? `${log.response_time_ms}ms` : '-'}
                </span>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>

      {/* Client Information */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Laptop className="h-5 w-5" />
            Client Information
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
            <div className="space-y-4">
              <div>
                <div className="flex items-center gap-2 mb-2">
                  <MapPin className="h-4 w-4 text-muted-foreground" />
                  <p className="text-sm font-medium">IP Address</p>
                </div>
                {log.client_ip ? (
                  <RouterLink
                    to={`/ip/${log.client_ip}`}
                    className="text-sm text-muted-foreground font-mono hover:text-primary underline"
                  >
                    {log.client_ip}
                  </RouterLink>
                ) : (
                  <p className="text-sm text-muted-foreground font-mono">-</p>
                )}
                {log.ip_geolocation_id && (
                  <Badge variant="outline" className="mt-1">
                    Geolocation Available
                  </Badge>
                )}
              </div>
            </div>

            <div className="space-y-4">
              <div>
                <div className="flex items-center gap-2 mb-2">
                  {getDeviceIcon(log.device_type)}
                  <p className="text-sm font-medium">Device</p>
                </div>
                <p className="text-sm text-muted-foreground capitalize">
                  {log.device_type || '-'}
                </p>
              </div>
              {log.operating_system && (
                <div>
                  <p className="text-sm font-medium">Operating System</p>
                  <p className="text-sm text-muted-foreground">
                    {log.operating_system}
                  </p>
                </div>
              )}
            </div>

            <div className="space-y-4">
              {log.browser && (
                <div>
                  <div className="flex items-center gap-2 mb-2">
                    <Globe className="h-4 w-4 text-muted-foreground" />
                    <p className="text-sm font-medium">Browser</p>
                  </div>
                  <p className="text-sm text-muted-foreground">
                    {log.browser}
                    {log.browser_version && ` ${log.browser_version}`}
                  </p>
                </div>
              )}
              {log.is_bot && (
                <div>
                  <div className="flex items-center gap-2 mb-2">
                    <Bot className="h-4 w-4 text-muted-foreground" />
                    <p className="text-sm font-medium">Bot Detection</p>
                  </div>
                  <Badge variant="secondary">
                    {log.bot_name || 'Detected as Bot'}
                  </Badge>
                </div>
              )}
            </div>
          </div>

          {log.user_agent && (
            <>
              <Separator className="my-4" />
              <div>
                <div className="flex items-center gap-2 mb-2">
                  <FileText className="h-4 w-4 text-muted-foreground" />
                  <p className="text-sm font-medium">User Agent</p>
                </div>
                <div className="bg-muted rounded-md p-3 font-mono text-xs break-all">
                  {log.user_agent}
                </div>
              </div>
            </>
          )}
        </CardContent>
      </Card>

      {/* Error Information */}
      {log.error_message && (
        <Card className="border-destructive">
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-destructive">
              <AlertCircle className="h-5 w-5" />
              Error Details
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="bg-destructive/10 border border-destructive/20 rounded-md p-4">
              <p className="font-mono text-sm">{log.error_message}</p>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}

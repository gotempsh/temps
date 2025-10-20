import { useParams, useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { getProxyLogByIdOptions } from '@/api/client/@tanstack/react-query.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ArrowLeft,
  Monitor,
  Smartphone,
  Bot,
  User,
  Activity,
  ExternalLink,
} from 'lucide-react'
import { format } from 'date-fns'
import { ProjectResponse } from '@/api/client'

interface RequestLogDetailProps {
  project: ProjectResponse
}

export default function RequestLogDetail({
  project: projectResponse,
}: RequestLogDetailProps) {
  const { logId } = useParams<{ logId: string }>()
  const navigate = useNavigate()

  const {
    data: logDetail,
    isLoading,
    error,
  } = useQuery({
    ...getProxyLogByIdOptions({
      path: {
        id: parseInt(logId || '0'),
      },
    }),
    enabled: !!logId,
  })

  const getStatusColor = (status: number) => {
    if (status >= 200 && status < 300) return 'bg-green-100 text-green-800'
    if (status >= 300 && status < 400) return 'bg-yellow-100 text-yellow-800'
    if (status >= 400 && status < 500) return 'bg-orange-100 text-orange-800'
    if (status >= 500) return 'bg-red-100 text-red-800'
    return 'bg-gray-100 text-gray-800'
  }

  const getMethodColor = (method: string) => {
    switch (method) {
      case 'GET':
        return 'bg-blue-100 text-blue-800'
      case 'POST':
        return 'bg-green-100 text-green-800'
      case 'PUT':
      case 'PATCH':
        return 'bg-yellow-100 text-yellow-800'
      case 'DELETE':
        return 'bg-red-100 text-red-800'
      default:
        return 'bg-gray-100 text-gray-800'
    }
  }

  const handleBack = () => {
    navigate(`/projects/${projectResponse.slug}/logs`)
  }

  if (error) {
    return (
      <div className="container mx-auto py-6">
        <Card>
          <CardContent className="pt-6">
            <div className="text-center">
              <p className="text-red-600">Failed to load log details</p>
              <Button onClick={handleBack} className="mt-4">
                Back to Logs
              </Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="container mx-auto py-6 space-y-4">
      <div className="flex items-center gap-4">
        <Button onClick={handleBack} variant="ghost" size="sm">
          <ArrowLeft className="h-4 w-4 mr-2" />
          Back to Logs
        </Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Request Log Detail</CardTitle>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="space-y-4">
              <Skeleton className="h-8 w-full" />
              <Skeleton className="h-32 w-full" />
              <Skeleton className="h-32 w-full" />
            </div>
          ) : logDetail ? (
            <div className="space-y-6">
              {/* Request & Response Overview */}
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
                {/* Request Information */}
                <Card>
                  <CardHeader>
                    <CardTitle className="text-base flex items-center gap-2">
                      <Activity className="h-4 w-4" />
                      Request Information
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-4">
                    <div className="grid grid-cols-2 gap-4">
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Method
                        </h4>
                        <Badge className={getMethodColor(logDetail.method)}>
                          {logDetail.method}
                        </Badge>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Status
                        </h4>
                        <Badge
                          className={getStatusColor(logDetail.status_code)}
                        >
                          {logDetail.status_code}
                        </Badge>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Duration
                        </h4>
                        <p className="text-sm font-medium">
                          {logDetail.response_time_ms
                            ? `${logDetail.response_time_ms}ms`
                            : 'N/A'}
                        </p>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Timestamp
                        </h4>
                        <p className="text-sm">
                          {format(
                            new Date(logDetail.timestamp),
                            'yyyy-MM-dd HH:mm:ss'
                          )}
                        </p>
                      </div>
                    </div>
                    <div className="space-y-2">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Full URL
                      </h4>
                      <div className="p-3 bg-muted rounded-md">
                        <p className="text-sm font-mono break-all">
                          https://{logDetail.host}
                          {logDetail.path}
                          {logDetail.query_string
                            ? `?${logDetail.query_string}`
                            : ''}
                        </p>
                      </div>
                    </div>
                    {logDetail.referrer && (
                      <div className="space-y-2">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Referrer
                        </h4>
                        <div className="p-3 bg-muted rounded-md">
                          <p className="text-sm break-all flex items-center gap-2">
                            {logDetail.referrer}
                            <a
                              href={logDetail.referrer}
                              target="_blank"
                              rel="noopener noreferrer"
                            >
                              <ExternalLink className="h-3 w-3" />
                            </a>
                          </p>
                        </div>
                      </div>
                    )}
                    {logDetail.error_message && (
                      <div className="space-y-2">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Error Message
                        </h4>
                        <div className="p-3 bg-muted rounded-md">
                          <p className="text-sm">{logDetail.error_message}</p>
                        </div>
                      </div>
                    )}
                  </CardContent>
                </Card>

                {/* Response & Deployment Info */}
                <Card>
                  <CardHeader>
                    <CardTitle className="text-base flex items-center gap-2">
                      <Activity className="h-4 w-4" />
                      Routing & Deployment
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-4">
                    <div className="grid grid-cols-2 gap-4">
                      {logDetail.environment_id && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Environment ID
                          </h4>
                          <p className="text-sm">{logDetail.environment_id}</p>
                        </div>
                      )}
                      {logDetail.deployment_id && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Deployment ID
                          </h4>
                          <p className="text-sm">{logDetail.deployment_id}</p>
                        </div>
                      )}
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Routing Status
                        </h4>
                        <Badge variant="outline">
                          {logDetail.routing_status}
                        </Badge>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Request Source
                        </h4>
                        <Badge variant="outline">
                          {logDetail.request_source}
                        </Badge>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Request ID
                        </h4>
                        <p className="text-sm font-mono break-all">
                          {logDetail.request_id}
                        </p>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          System Request
                        </h4>
                        <Badge
                          variant={
                            logDetail.is_system_request
                              ? 'default'
                              : 'secondary'
                          }
                        >
                          {logDetail.is_system_request ? 'Yes' : 'No'}
                        </Badge>
                      </div>
                    </div>
                    {(logDetail.cache_status ||
                      logDetail.upstream_host ||
                      logDetail.container_id) && (
                      <div className="grid grid-cols-2 gap-4">
                        {logDetail.cache_status && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              Cache Status
                            </h4>
                            <Badge variant="outline">
                              {logDetail.cache_status}
                            </Badge>
                          </div>
                        )}
                        {logDetail.upstream_host && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              Upstream Host
                            </h4>
                            <p className="text-sm font-mono break-all">
                              {logDetail.upstream_host}
                            </p>
                          </div>
                        )}
                        {logDetail.container_id && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              Container ID
                            </h4>
                            <p className="text-sm font-mono break-all">
                              {logDetail.container_id}
                            </p>
                          </div>
                        )}
                      </div>
                    )}
                  </CardContent>
                </Card>
              </div>

              {/* Visitor Information Section */}
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
                {/* Visitor Information */}
                <Card>
                  <CardHeader>
                    <CardTitle className="text-base flex items-center gap-2">
                      <User className="h-4 w-4" />
                      Visitor Information
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-4">
                    <div className="grid grid-cols-2 gap-4">
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          IP Address
                        </h4>
                        <p className="text-sm font-mono">
                          {logDetail.client_ip || 'N/A'}
                        </p>
                      </div>
                      {logDetail.device_type && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Device Type
                          </h4>
                          <Badge variant="outline">
                            {logDetail.device_type === 'mobile' ? (
                              <>
                                <Smartphone className="h-3 w-3 mr-1" /> Mobile
                              </>
                            ) : logDetail.device_type === 'tablet' ? (
                              <>
                                <Smartphone className="h-3 w-3 mr-1" /> Tablet
                              </>
                            ) : (
                              <>
                                <Monitor className="h-3 w-3 mr-1" /> Desktop
                              </>
                            )}
                          </Badge>
                        </div>
                      )}
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Is Bot
                        </h4>
                        <Badge
                          variant={logDetail.is_bot ? 'destructive' : 'default'}
                        >
                          {logDetail.is_bot ? (
                            <>
                              <Bot className="h-3 w-3 mr-1" /> Yes
                            </>
                          ) : (
                            'No'
                          )}
                        </Badge>
                      </div>
                      {logDetail.bot_name && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Bot Name
                          </h4>
                          <p className="text-sm">{logDetail.bot_name}</p>
                        </div>
                      )}
                    </div>
                    {logDetail.browser && (
                      <div className="space-y-2">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Browser
                        </h4>
                        <p className="text-sm">
                          {logDetail.browser}{' '}
                          {logDetail.browser_version &&
                            `v${logDetail.browser_version}`}
                        </p>
                      </div>
                    )}
                    {logDetail.operating_system && (
                      <div className="space-y-2">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Operating System
                        </h4>
                        <p className="text-sm">{logDetail.operating_system}</p>
                      </div>
                    )}
                    <div className="space-y-2">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        User Agent
                      </h4>
                      <div className="p-3 bg-muted rounded-md">
                        <p className="text-xs font-mono break-all">
                          {logDetail.user_agent || 'N/A'}
                        </p>
                      </div>
                    </div>
                  </CardContent>
                </Card>

                {/* Request Sizes */}
                {(logDetail.request_size_bytes ||
                  logDetail.response_size_bytes) && (
                  <Card>
                    <CardHeader>
                      <CardTitle className="text-base flex items-center gap-2">
                        <Activity className="h-4 w-4" />
                        Request & Response Size
                      </CardTitle>
                    </CardHeader>
                    <CardContent className="space-y-4">
                      <div className="grid grid-cols-2 gap-4">
                        {logDetail.request_size_bytes && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              Request Size
                            </h4>
                            <p className="text-sm">
                              {(logDetail.request_size_bytes / 1024).toFixed(2)}{' '}
                              KB
                            </p>
                          </div>
                        )}
                        {logDetail.response_size_bytes && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              Response Size
                            </h4>
                            <p className="text-sm">
                              {(logDetail.response_size_bytes / 1024).toFixed(
                                2
                              )}{' '}
                              KB
                            </p>
                          </div>
                        )}
                      </div>
                    </CardContent>
                  </Card>
                )}
              </div>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              No log data available
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

import { useEffect } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { getRequestLogByIdOptions } from '@/api/client/@tanstack/react-query.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ScrollArea } from '@/components/ui/scroll-area'
import {
  ArrowLeft,
  Monitor,
  Smartphone,
  Bot,
  User,
  MapPin,
  Activity,
  GitBranch,
  GitCommit,
  ExternalLink,
} from 'lucide-react'
import { format } from 'date-fns'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export default function GeneralRequestLogDetail() {
  const { logId } = useParams<{ logId: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  usePageTitle('Request Log Detail')

  const {
    data: logDetail,
    isLoading,
    error,
  } = useQuery({
    ...getRequestLogByIdOptions({
      path: {
        id: parseInt(logId || '0'),
      },
      query: {
        // Don't pass project_id to fetch from all system logs
      },
    }),
    enabled: !!logId,
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Proxy Logs', href: '/proxy-logs' },
      { label: 'Request Log Detail' },
    ])
  }, [setBreadcrumbs])

  const getStatusColor = (status: number) => {
    if (status >= 200 && status < 300)
      return 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
    if (status >= 300 && status < 400)
      return 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200'
    if (status >= 400 && status < 500)
      return 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200'
    if (status >= 500)
      return 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
    return 'bg-gray-100 text-gray-800 dark:bg-gray-700 dark:text-gray-200'
  }

  const getMethodColor = (method: string) => {
    switch (method) {
      case 'GET':
        return 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200'
      case 'POST':
        return 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200'
      case 'PUT':
      case 'PATCH':
        return 'bg-yellow-100 text-yellow-800 dark:bg-yellow-900 dark:text-yellow-200'
      case 'DELETE':
        return 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
      default:
        return 'bg-gray-100 text-gray-800 dark:bg-gray-700 dark:text-gray-200'
    }
  }

  const handleBack = () => {
    navigate('/proxy-logs')
  }

  if (error) {
    return (
      <div className="container max-w-7xl mx-auto py-6">
        <Card>
          <CardContent className="pt-6">
            <div className="text-center">
              <p className="text-red-600">Failed to load log details</p>
              <Button onClick={handleBack} className="mt-4">
                Back to Proxy Logs
              </Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="container max-w-7xl mx-auto py-6 space-y-4">
      <div className="flex items-center gap-4">
        <Button onClick={handleBack} variant="ghost" size="sm">
          <ArrowLeft className="h-4 w-4 mr-2" />
          Back to Proxy Logs
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
                          {logDetail.elapsed_time}ms
                        </p>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Timestamp
                        </h4>
                        <p className="text-sm">
                          {logDetail.finished_at &&
                            format(
                              new Date(logDetail.finished_at),
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
                          {logDetail.request_path}
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
                    {logDetail.message && (
                      <div className="space-y-2">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Log Message
                        </h4>
                        <div className="p-3 bg-muted rounded-md">
                          <p className="text-sm">{logDetail.message}</p>
                        </div>
                      </div>
                    )}
                  </CardContent>
                </Card>

                {/* Response & Deployment Info */}
                <Card>
                  <CardHeader>
                    <CardTitle className="text-base flex items-center gap-2">
                      <GitBranch className="h-4 w-4" />
                      Deployment & Response
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-4">
                    <div className="grid grid-cols-2 gap-4">
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Project ID
                        </h4>
                        <p className="text-sm">{logDetail.project_id}</p>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Environment ID
                        </h4>
                        <p className="text-sm">{logDetail.environment_id}</p>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Deployment ID
                        </h4>
                        <p className="text-sm">{logDetail.deployment_id}</p>
                      </div>
                      {logDetail.branch && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Branch
                          </h4>
                          <Badge variant="outline">
                            <GitBranch className="h-3 w-3 mr-1" />
                            {logDetail.branch}
                          </Badge>
                        </div>
                      )}
                      {logDetail.commit && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Commit
                          </h4>
                          <Badge variant="outline">
                            <GitCommit className="h-3 w-3 mr-1" />
                            <span className="font-mono">
                              {logDetail.commit.substring(0, 7)}
                            </span>
                          </Badge>
                        </div>
                      )}
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Log Level
                        </h4>
                        <Badge
                          variant={
                            logDetail.level === 'error'
                              ? 'destructive'
                              : 'secondary'
                          }
                        >
                          {logDetail.level}
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
                    </div>
                    <div className="grid grid-cols-2 gap-4">
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Is Entry Page
                        </h4>
                        <Badge
                          variant={
                            logDetail.is_entry_page ? 'default' : 'secondary'
                          }
                        >
                          {logDetail.is_entry_page ? 'Yes' : 'No'}
                        </Badge>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Is Static File
                        </h4>
                        <Badge
                          variant={
                            logDetail.is_static_file ? 'default' : 'secondary'
                          }
                        >
                          {logDetail.is_static_file ? 'Yes' : 'No'}
                        </Badge>
                      </div>
                    </div>
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
                          {logDetail.ip_address || 'N/A'}
                        </p>
                      </div>
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Device Type
                        </h4>
                        <Badge variant="outline">
                          {logDetail.is_mobile ? (
                            <>
                              <Smartphone className="h-3 w-3 mr-1" /> Mobile
                            </>
                          ) : (
                            <>
                              <Monitor className="h-3 w-3 mr-1" /> Desktop
                            </>
                          )}
                        </Badge>
                      </div>
                      {logDetail.visitor_id && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Visitor ID
                          </h4>
                          <p className="text-sm font-mono text-xs break-all">
                            {logDetail.visitor_id}
                          </p>
                        </div>
                      )}
                      {logDetail.session_id && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Session ID
                          </h4>
                          <p className="text-sm">{logDetail.session_id}</p>
                        </div>
                      )}
                      <div className="space-y-1">
                        <h4 className="text-sm font-medium text-muted-foreground">
                          Is Crawler
                        </h4>
                        <Badge
                          variant={
                            logDetail.is_crawler ? 'destructive' : 'default'
                          }
                        >
                          {logDetail.is_crawler ? (
                            <>
                              <Bot className="h-3 w-3 mr-1" /> Yes
                            </>
                          ) : (
                            'No'
                          )}
                        </Badge>
                      </div>
                      {logDetail.crawler_name && (
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Crawler Name
                          </h4>
                          <p className="text-sm">{logDetail.crawler_name}</p>
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

                {/* Geolocation */}
                {logDetail.ip_geolocation && (
                  <Card>
                    <CardHeader>
                      <CardTitle className="text-base flex items-center gap-2">
                        <MapPin className="h-4 w-4" />
                        Geolocation
                      </CardTitle>
                    </CardHeader>
                    <CardContent className="space-y-4">
                      <div className="grid grid-cols-2 gap-4">
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Country
                          </h4>
                          <p className="text-sm">
                            {logDetail.ip_geolocation.country}
                          </p>
                        </div>
                        {logDetail.ip_geolocation.region && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              Region
                            </h4>
                            <p className="text-sm">
                              {logDetail.ip_geolocation.region}
                            </p>
                          </div>
                        )}
                        {logDetail.ip_geolocation.city && (
                          <div className="space-y-1">
                            <h4 className="text-sm font-medium text-muted-foreground">
                              City
                            </h4>
                            <p className="text-sm">
                              {logDetail.ip_geolocation.city}
                            </p>
                          </div>
                        )}
                        <div className="space-y-1">
                          <h4 className="text-sm font-medium text-muted-foreground">
                            Coordinates
                          </h4>
                          <p className="text-xs font-mono">
                            {logDetail.ip_geolocation.latitude},{' '}
                            {logDetail.ip_geolocation.longitude}
                          </p>
                        </div>
                      </div>
                    </CardContent>
                  </Card>
                )}
              </div>

              {/* Headers Section */}
              <Tabs defaultValue="request-headers" className="w-full">
                <TabsList className="grid w-full grid-cols-2">
                  <TabsTrigger value="request-headers">
                    Request Headers
                  </TabsTrigger>
                  <TabsTrigger value="response-headers">
                    Response Headers
                  </TabsTrigger>
                </TabsList>

                <TabsContent value="request-headers" className="mt-6">
                  <Card>
                    <CardHeader>
                      <CardTitle className="text-base">
                        Request Headers
                      </CardTitle>
                    </CardHeader>
                    <CardContent>
                      <ScrollArea className="h-[400px] w-full">
                        {logDetail.request_headers ? (
                          <div className="space-y-3">
                            {(() => {
                              try {
                                const headers =
                                  typeof logDetail.request_headers === 'string'
                                    ? JSON.parse(logDetail.request_headers)
                                    : logDetail.request_headers
                                return Object.entries(headers).map(
                                  ([key, value]) => (
                                    <div
                                      key={key}
                                      className="border-b pb-2 last:border-0"
                                    >
                                      <div className="flex flex-col space-y-1">
                                        <span className="text-sm font-medium">
                                          {key}
                                        </span>
                                        <span className="text-sm text-muted-foreground font-mono break-all">
                                          {Array.isArray(value)
                                            ? value.join(', ')
                                            : String(value)}
                                        </span>
                                      </div>
                                    </div>
                                  )
                                )
                              } catch (e) {
                                return (
                                  <p className="text-sm text-muted-foreground">
                                    Failed to parse request headers
                                  </p>
                                )
                              }
                            })()}
                          </div>
                        ) : (
                          <p className="text-sm text-muted-foreground">
                            No request headers available
                          </p>
                        )}
                      </ScrollArea>
                    </CardContent>
                  </Card>
                </TabsContent>

                <TabsContent value="response-headers" className="mt-6">
                  <Card>
                    <CardHeader>
                      <CardTitle className="text-base">
                        Response Headers
                      </CardTitle>
                    </CardHeader>
                    <CardContent>
                      <ScrollArea className="h-[400px] w-full">
                        {logDetail.headers ? (
                          <div className="space-y-3">
                            {(() => {
                              try {
                                const headers =
                                  typeof logDetail.headers === 'string'
                                    ? JSON.parse(logDetail.headers)
                                    : logDetail.headers
                                return Object.entries(headers).map(
                                  ([key, value]) => (
                                    <div
                                      key={key}
                                      className="border-b pb-2 last:border-0"
                                    >
                                      <div className="flex flex-col space-y-1">
                                        <span className="text-sm font-medium">
                                          {key}
                                        </span>
                                        <span className="text-sm text-muted-foreground font-mono break-all">
                                          {Array.isArray(value)
                                            ? value.join(', ')
                                            : String(value)}
                                        </span>
                                      </div>
                                    </div>
                                  )
                                )
                              } catch (e) {
                                return (
                                  <p className="text-sm text-muted-foreground">
                                    Failed to parse response headers
                                  </p>
                                )
                              }
                            })()}
                          </div>
                        ) : (
                          <p className="text-sm text-muted-foreground">
                            No response headers available
                          </p>
                        )}
                      </ScrollArea>
                    </CardContent>
                  </Card>
                </TabsContent>
              </Tabs>
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

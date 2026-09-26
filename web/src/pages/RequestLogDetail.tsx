// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useParams, useNavigate, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import {
  getProxyLogByIdOptions,
  getProxyLogByRequestIdOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { httpStatusClass } from '@/lib/http-status-class'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  fmtDateTime,
  fmtBytes,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import {
  Activity,
  AlertCircle,
  ArrowLeft,
  Bot,
  ExternalLink,
  Monitor,
  Smartphone,
  User,
} from 'lucide-react'
import { ProjectResponse } from '@/api/client'

interface RequestLogDetailProps {
  project: ProjectResponse
}

// Tone from the HTTP status class — the Detail template's single verdict.
// Color only ever appears through `Status`, never a hand-picked bg-*/text-*
// pair, so this replaces the old getStatusColor()/getMethodColor() helpers.
function statusVerdict(statusCode: number): { tone: StatusTone; label: string } {
  switch (httpStatusClass(statusCode)) {
    case '2xx':
      return { tone: 'ok', label: String(statusCode) }
    case '3xx':
      return { tone: 'idle', label: String(statusCode) }
    case '4xx':
      return { tone: 'warn', label: String(statusCode) }
    case '5xx':
      return { tone: 'error', label: String(statusCode) }
    default:
      return { tone: 'idle', label: String(statusCode) }
  }
}

function RequestLogDetailSkeleton({ backAction }: { backAction: React.ReactNode }) {
  return (
    <Detail
      title={<div className="h-7 w-56 animate-pulse rounded bg-muted" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <div className="h-3 w-16 animate-pulse rounded bg-muted" />,
        value: <div className="h-4 w-24 animate-pulse rounded bg-muted" />,
      }))}
      main={
        <>
          <div className="h-48 w-full animate-pulse rounded-lg bg-muted" />
          <div className="h-40 w-full animate-pulse rounded-lg bg-muted" />
        </>
      }
      aside={<div className="h-56 w-full animate-pulse rounded-lg bg-muted" />}
    />
  )
}

export default function RequestLogDetail({
  project: projectResponse,
}: RequestLogDetailProps) {
  const { logId } = useParams<{ logId: string }>()
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  // Row timestamp forwarded by the list; bounds the backend's hypertable
  // lookup. Absent on bare deep-links, which fall back to a wider scan.
  const ts = searchParams.get('ts')

  // The list navigates by request_id (resolves under both the TimescaleDB
  // and ClickHouse backends — the latter has no serial id column). Purely
  // numeric params are legacy serial-id deep-links and keep using the by-id
  // endpoint.
  const isLegacyNumericId = /^\d+$/.test(logId || '')

  const byId = useQuery({
    ...getProxyLogByIdOptions({
      path: {
        id: parseInt(logId || '0'),
      },
      query: { timestamp: ts ?? undefined, project_id: projectResponse.id },
    }),
    enabled: !!logId && isLegacyNumericId,
  })
  const byRequestId = useQuery({
    ...getProxyLogByRequestIdOptions({
      path: {
        request_id: logId || '',
      },
      query: { timestamp: ts ?? undefined, project_id: projectResponse.id },
    }),
    enabled: !!logId && !isLegacyNumericId,
  })
  const {
    data: logDetail,
    isLoading,
    error,
    refetch,
  } = isLegacyNumericId ? byId : byRequestId

  const handleBack = () => {
    navigate(`/projects/${projectResponse.slug}/logs`)
  }

  const backAction = (
    <Button onClick={handleBack} variant="ghost" size="sm">
      <ArrowLeft className="h-4 w-4 mr-2" />
      Back to Logs
    </Button>
  )

  if (isLoading) {
    return <RequestLogDetailSkeleton backAction={backAction} />
  }

  if (error || !logDetail) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Couldn't load log details"
        description={
          error
            ? 'The request log could not be loaded.'
            : 'No log data is available for this request.'
        }
        action={
          error ? (
            <Button onClick={() => void refetch()}>Retry</Button>
          ) : (
            backAction
          )
        }
      />
    )
  }

  const verdict = statusVerdict(logDetail.status_code)

  const facts: DetailFact[] = [
    { label: 'Method', value: <Badge variant="outline">{logDetail.method}</Badge> },
    {
      label: 'Duration',
      value: logDetail.response_time_ms ? `${logDetail.response_time_ms}ms` : 'N/A',
    },
    { label: 'Timestamp', value: fmtDateTime(logDetail.timestamp) },
    { label: 'Routing status', value: <Badge variant="outline">{logDetail.routing_status}</Badge> },
    { label: 'Request source', value: <Badge variant="outline">{logDetail.request_source}</Badge> },
  ]

  const hasSizes = Boolean(
    logDetail.request_size_bytes || logDetail.response_size_bytes
  )
  const hasRoutingExtras = Boolean(
    logDetail.cache_status || logDetail.upstream_host || logDetail.container_id
  )

  return (
    <Detail
      title="Request log"
      description={
        <span className="font-mono text-xs break-all">{logDetail.request_id}</span>
      }
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={backAction}
      facts={facts}
      main={
        <>
          {logDetail.error_message ? (
            <Callout tone="error" title="Error message">
              {logDetail.error_message}
            </Callout>
          ) : null}

          <Card>
            <CardHeader>
              <CardTitle className="text-base flex items-center gap-2">
                <Activity className="h-4 w-4" />
                Request Information
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-2">
                <h4 className="text-sm font-medium text-muted-foreground">
                  Full URL
                </h4>
                <div className="p-3 bg-muted rounded-md">
                  <p className="text-sm font-mono break-all">
                    https://{logDetail.host}
                    {logDetail.path}
                    {logDetail.query_string ? `?${logDetail.query_string}` : ''}
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
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="text-base flex items-center gap-2">
                <Activity className="h-4 w-4" />
                Routing & Deployment
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
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
                    System Request
                  </h4>
                  <Badge variant={logDetail.is_system_request ? 'default' : 'secondary'}>
                    {logDetail.is_system_request ? 'Yes' : 'No'}
                  </Badge>
                </div>
              </div>
              {hasRoutingExtras && (
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                  {logDetail.cache_status && (
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Cache Status
                      </h4>
                      <Badge variant="outline">{logDetail.cache_status}</Badge>
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
        </>
      }
      aside={
        <>
          <Card>
            <CardHeader>
              <CardTitle className="text-base flex items-center gap-2">
                <User className="h-4 w-4" />
                Visitor Information
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                <div className="space-y-1">
                  <h4 className="text-sm font-medium text-muted-foreground">
                    IP Address
                  </h4>
                  <p className="text-sm font-mono">{logDetail.client_ip || 'N/A'}</p>
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
                  <Badge variant={logDetail.is_bot ? 'destructive' : 'default'}>
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
                    {logDetail.browser_version && `v${logDetail.browser_version}`}
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

          {hasSizes && (
            <Card>
              <CardHeader>
                <CardTitle className="text-base flex items-center gap-2">
                  <Activity className="h-4 w-4" />
                  Request & Response Size
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-4">
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                  {logDetail.request_size_bytes && (
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Request Size
                      </h4>
                      <p className="text-sm">{fmtBytes(logDetail.request_size_bytes)}</p>
                    </div>
                  )}
                  {logDetail.response_size_bytes && (
                    <div className="space-y-1">
                      <h4 className="text-sm font-medium text-muted-foreground">
                        Response Size
                      </h4>
                      <p className="text-sm">{fmtBytes(logDetail.response_size_bytes)}</p>
                    </div>
                  )}
                </div>
              </CardContent>
            </Card>
          )}
        </>
      }
    />
  )
}

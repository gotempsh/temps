import { getServiceOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { ServiceHealthBadge } from '@/components/storage/ServiceHealthCard'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  type LogLevel,
  type LogSearchLine,
  useLogHistory,
} from '@/hooks/useLogHistory'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  Database,
  RefreshCw,
  ScrollText,
  Server,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'

/** Tailwind classes per normalized level — mirrors the deployment log viewer. */
const LEVEL_CLASS: Record<LogLevel, string> = {
  ERROR: 'text-red-500',
  WARN: 'text-amber-500',
  INFO: 'text-foreground',
  DEBUG: 'text-muted-foreground',
  TRACE: 'text-muted-foreground/70',
}

const LEVELS: LogLevel[] = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE']

function formatTs(ts: string): string {
  const d = new Date(ts)
  if (Number.isNaN(d.getTime())) return ts
  return d.toISOString().replace('T', ' ').replace('Z', '')
}

/**
 * Persisted, searchable log history for an imported/managed external service
 * (Postgres, MariaDB, Redis, MongoDB, MinIO). Reuses the same log-aggregator
 * search pipeline as application/deployment logs, scoped by
 * `external_service_id` instead of a project. Wears the same header shell as
 * the service detail page so it reads as one app.
 */
export function ServiceLogs() {
  const { id } = useParams<{ id: string }>()
  const serviceId = id ? parseInt(id, 10) : NaN

  const { data: service, isLoading: serviceLoading } = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: !Number.isNaN(serviceId),
  })

  const [text, setText] = useState('')
  const [activeLevels, setActiveLevels] = useState<LogLevel[]>([])

  // Look back 24h by default — matches the service history window operators expect.
  const startTime = useMemo(
    () => new Date(Date.now() - 24 * 60 * 60 * 1000).toISOString(),
    []
  )

  const { data, isLoading, isFetching, error, refetch } = useLogHistory(
    {
      // projectId is ignored server-side when externalServiceId is set.
      projectId: 0,
      externalServiceId: serviceId,
      startTime,
      levels: activeLevels.length ? activeLevels : undefined,
      text: text.trim() || undefined,
      pageSize: 500,
    },
    !Number.isNaN(serviceId)
  )

  const lines: LogSearchLine[] = data?.lines ?? []
  const svc = service?.service

  function toggleLevel(level: LogLevel) {
    setActiveLevels((prev) =>
      prev.includes(level) ? prev.filter((l) => l !== level) : [...prev, level]
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="p-4 space-y-6 md:p-6">
        {/* Header — mirrors ServiceDetail so the Logs view reads as one app. */}
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <Link to={`/storage/${id}`}>
              <Button variant="ghost" size="icon">
                <ArrowLeft className="h-4 w-4" />
              </Button>
            </Link>
            {svc ? (
              <ServiceLogo service={svc.service_type} className="h-8 w-8" />
            ) : (
              <ScrollText className="h-8 w-8 text-muted-foreground" />
            )}
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2 flex-wrap">
                <h1 className="text-xl font-semibold sm:text-2xl">
                  {svc?.name ?? id}
                </h1>
                {svc ? (
                  <>
                    <Badge
                      variant={
                        svc.status === 'running'
                          ? 'default'
                          : svc.status === 'stopped'
                            ? 'secondary'
                            : 'outline'
                      }
                      className="capitalize"
                    >
                      {svc.status}
                    </Badge>
                    {svc.status === 'running' ? (
                      <ServiceHealthBadge serviceId={serviceId} />
                    ) : null}
                    <Badge variant="outline" className="gap-1.5">
                      <ServiceLogo
                        service={svc.service_type}
                        className="h-3 w-3"
                      />
                      {svc.service_type}
                    </Badge>
                  </>
                ) : null}
                <Badge variant="outline" className="gap-1.5">
                  <ScrollText className="h-3 w-3" />
                  Logs
                </Badge>
              </div>
              {svc ? (
                <p className="text-sm text-muted-foreground">
                  Created <TimeAgo date={svc.created_at} />
                </p>
              ) : null}
            </div>
          </div>

          {/* Same action row as the detail page; Monitoring/Browse Data link
              back out, and Refresh replaces the kebab for this view. */}
          <div className="flex items-center gap-2 self-start sm:self-auto flex-wrap">
            {svc?.status === 'running' ? (
              <Link to={`/storage/${id}/monitoring`}>
                <Button variant="outline" size="sm" className="gap-2">
                  <Server className="h-4 w-4" />
                  <span className="hidden sm:inline">Monitoring</span>
                </Button>
              </Link>
            ) : null}
            <Link to={`/storage/${id}/browse`}>
              <Button variant="outline" size="sm" className="gap-2">
                <Database className="h-4 w-4" />
                <span className="hidden sm:inline">Browse Data</span>
              </Button>
            </Link>
            <Button
              variant="outline"
              size="sm"
              className="gap-2"
              onClick={() => refetch()}
              disabled={isFetching}
            >
              <RefreshCw
                className={`h-4 w-4 ${isFetching ? 'animate-spin' : ''}`}
              />
              <span className="hidden sm:inline">Refresh</span>
            </Button>
          </div>
        </div>

        {/* Filter bar */}
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:flex-wrap">
          <Input
            placeholder="Filter logs (text search)…"
            value={text}
            onChange={(e) => setText(e.target.value)}
            className="w-full sm:w-[320px]"
          />
          <div className="flex flex-wrap gap-1">
            {LEVELS.map((level) => (
              <Badge
                key={level}
                variant={activeLevels.includes(level) ? 'default' : 'outline'}
                className="cursor-pointer select-none"
                onClick={() => toggleLevel(level)}
              >
                {level}
              </Badge>
            ))}
          </div>
          <span className="text-xs text-muted-foreground sm:ml-auto">
            Last 24h · {lines.length} line{lines.length === 1 ? '' : 's'}
          </span>
        </div>

        {/* Log panel — bounded height + its own scrollbar so the page chrome
            (header/filter) stays put and long output scrolls inside. */}
        {error ? (
          <div className="rounded-md border border-destructive/40 bg-destructive/5 p-4 text-sm text-destructive">
            Failed to load logs: {(error as Error).message}
          </div>
        ) : isLoading || serviceLoading ? (
          <div className="space-y-2">
            {Array.from({ length: 12 }).map((_, i) => (
              <Skeleton key={i} className="h-5 w-full" />
            ))}
          </div>
        ) : lines.length === 0 ? (
          <div className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
            No logs found in the last 24 hours. Logs appear here once the
            service container emits output.
          </div>
        ) : (
          <div className="rounded-md border bg-muted/30">
            <div className="max-h-[calc(100vh-19rem)] overflow-auto">
              <pre className="min-w-full p-3 font-mono text-xs leading-relaxed">
                {lines.map((line, i) => (
                  <div
                    key={`${line.chunk_id}-${line.line_offset}-${i}`}
                    className="flex gap-3 border-b border-border/40 py-0.5 last:border-0"
                  >
                    <span className="shrink-0 text-muted-foreground/70">
                      {formatTs(line.timestamp)}
                    </span>
                    <span
                      className={`shrink-0 w-12 ${LEVEL_CLASS[line.level]}`}
                    >
                      {line.level}
                    </span>
                    <span
                      className={`whitespace-pre-wrap break-all ${LEVEL_CLASS[line.level]}`}
                    >
                      {line.message}
                    </span>
                  </div>
                ))}
              </pre>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

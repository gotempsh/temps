import {
  getServiceHealthStatus,
  triggerServiceHealthCheck,
  type HealthStatus,
  type ServiceHealthResponse,
} from '@/lib/service-health'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, Loader2, RefreshCcw } from 'lucide-react'
import { toast } from 'sonner'

/**
 * Map backend status to color tokens used in the badge + sparkline bars.
 */
function statusColor(status: HealthStatus | 'unknown'): string {
  switch (status) {
    case 'operational':
      return 'bg-green-500'
    case 'degraded':
      return 'bg-amber-500'
    case 'down':
      return 'bg-red-500'
    default:
      return 'bg-muted'
  }
}

function statusLabel(status?: HealthStatus | null): string {
  if (!status) return 'Pending check'
  if (status === 'operational') return 'Operational'
  if (status === 'degraded') return 'Degraded'
  return 'Down'
}

/**
 * Inline green/amber/red dot + label. Drops into the header next to the
 * "Running" pill on ServiceDetail.
 */
export function ServiceHealthBadge({
  serviceId,
  className,
}: {
  serviceId: number
  className?: string
}) {
  const { data, isLoading } = useQuery<ServiceHealthResponse>({
    queryKey: ['service-health', serviceId],
    queryFn: () => getServiceHealthStatus(serviceId, 50),
    refetchInterval: 30_000,
    staleTime: 25_000,
  })

  if (isLoading && !data) {
    return null
  }

  const status = data?.status ?? null

  return (
    <span
      className={
        'inline-flex items-center gap-1.5 rounded-full border border-border bg-background px-2.5 py-0.5 text-xs font-medium ' +
        (className ?? '')
      }
      title={data?.last_error ?? undefined}
    >
      <span className={`inline-block h-2 w-2 rounded-full ${statusColor(status ?? 'unknown')}`} />
      {statusLabel(status)}
    </span>
  )
}

/**
 * Compact one-row health strip. Status is already shown in the page header,
 * so this surface only adds the metrics the header can't fit: uptime,
 * response time, last-check timestamp, sparkline, and a manual trigger.
 * Expands into a destructive alert only when there's an active failure —
 * otherwise it stays thin and out of the way.
 */
export function ServiceHealthCard({ serviceId }: { serviceId: number }) {
  const queryClient = useQueryClient()
  const { data, isLoading, isError } = useQuery<ServiceHealthResponse>({
    queryKey: ['service-health', serviceId],
    queryFn: () => getServiceHealthStatus(serviceId, 50),
    refetchInterval: 30_000,
    staleTime: 25_000,
  })

  // Manual probe. Reuses the monitor's check logic server-side so the
  // consecutive-failure counter and alerts stay honest.
  const manualCheck = useMutation({
    mutationFn: () => triggerServiceHealthCheck(serviceId),
    onSuccess: (snapshot) => {
      queryClient.setQueryData(['service-health', serviceId], snapshot)
      queryClient.invalidateQueries({ queryKey: ['service-health-batch'] })
      toast.success(
        snapshot.status === 'operational'
          ? 'Service is operational'
          : snapshot.status === 'degraded'
            ? 'Service is degraded'
            : 'Service is down',
      )
    },
    onError: (err: Error) => {
      toast.error('Health check failed', { description: err.message })
    },
  })

  if (isLoading && !data) {
    return (
      <div className="flex h-10 items-center gap-2 px-1 text-xs text-muted-foreground">
        <Loader2 className="h-3.5 w-3.5 animate-spin" />
        Loading health…
      </div>
    )
  }

  if (isError || !data) {
    return null
  }

  const hasFailure =
    data.status === 'down' || (data.consecutive_failures ?? 0) > 0

  // Degraded is not a "failure" (the probe succeeded, so consecutive_failures
  // stays 0), but it still has a reason the user needs to see — e.g. "mongodb
  // responded in 10007ms (>2000ms)". Surface it as an amber notice so the
  // bare "Degraded" badge in the header is no longer unexplained.
  const isDegraded = !hasFailure && data.status === 'degraded'

  return (
    <div className="space-y-3">
      {/*
        One-row strip. Inline stats separated by a subtle divider so the whole
        thing reads as a single compact line instead of three card tiles.
      */}
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2 rounded-md border bg-muted/20 px-3 py-2 text-xs">
        <Stat label="Uptime 24h">
          {data.uptime_24h_percent != null
            ? `${data.uptime_24h_percent.toFixed(2)}%`
            : '—'}
        </Stat>
        <Stat label="Response">
          {data.response_time_ms != null ? `${data.response_time_ms} ms` : '—'}
        </Stat>
        {data.last_checked_at ? (
          <Stat label="Checked">
            <TimeAgo date={data.last_checked_at} />
          </Stat>
        ) : null}

        {data.recent_checks.length > 0 ? (
          <div
            className="ml-auto flex h-5 items-stretch gap-px overflow-hidden rounded-sm"
            aria-label={`Last ${data.recent_checks.length} health checks`}
          >
            {/* Backend returns DESC → reverse so newest is rightmost. */}
            {[...data.recent_checks].reverse().map((entry, idx) => (
              <span
                key={`${entry.checked_at}-${idx}`}
                className={`w-1 ${statusColor(entry.status)}`}
                title={`${new Date(entry.checked_at).toLocaleString()} — ${entry.status}${
                  entry.response_time_ms != null
                    ? ` (${entry.response_time_ms}ms)`
                    : ''
                }`}
              />
            ))}
          </div>
        ) : null}

        <Button
          variant="ghost"
          size="sm"
          className="h-7 gap-1.5 px-2 text-xs"
          onClick={() => manualCheck.mutate()}
          disabled={manualCheck.isPending}
          title="Run a health check right now"
        >
          {manualCheck.isPending ? (
            <Loader2 className="h-3 w-3 animate-spin" />
          ) : (
            <RefreshCcw className="h-3 w-3" />
          )}
          Check now
        </Button>
      </div>

      {hasFailure ? (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription className="space-y-1">
            <p className="font-medium">
              {data.consecutive_failures >= 3
                ? `Service has failed ${data.consecutive_failures} consecutive checks — an alert has been sent.`
                : `Service has failed ${data.consecutive_failures} check(s) in a row.`}
            </p>
            {data.last_error ? (
              <p className="break-words text-xs opacity-80">{data.last_error}</p>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : isDegraded ? (
        <Alert variant="warning">
          <AlertCircle className="h-4 w-4" />
          <AlertDescription className="space-y-1">
            <p className="font-medium">Service is degraded.</p>
            {data.last_error ? (
              <p className="break-words text-xs opacity-80">{data.last_error}</p>
            ) : (
              <p className="text-xs opacity-80">
                The last health check succeeded but the service responded
                slowly. Check load and network latency to this instance.
              </p>
            )}
          </AlertDescription>
        </Alert>
      ) : null}
    </div>
  )
}

function Stat({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <span className="inline-flex items-baseline gap-1.5">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-medium tabular-nums text-foreground">
        {children}
      </span>
    </span>
  )
}

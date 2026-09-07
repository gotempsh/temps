// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Instance-wide Cloud telemetry write status and the decommission signal
 * (ADR-041 §9).
 *
 * The single most important thing on this card is
 * `local_span_store_required`. A partial cutover yields **zero** resource win —
 * one project still writing locally keeps the entire span store running — and
 * operators will reasonably believe they have saved something before they have.
 * So the answer is stated first, derived server-side, and carries its own
 * reason rather than being implied by two project counts.
 *
 * It renders on an unlinked instance too, in an onboarding state, because a
 * feature that disappears when unconfigured is a feature nobody discovers.
 *
 * It also hosts the bulk **Cloud telemetry activation** section (ADR-042 §11).
 * That section is rendered in every branch of this card — including while the
 * status is still loading and when reading it failed — because "switch every
 * project to Cloud" must never be invisible just because an unrelated query is
 * unhappy.
 */

import { getCloudTelemetryStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import type { CloudTelemetryWriteStatusResponse } from '@/api/client/types.gen'
import { CloudTelemetryActivationSection } from '@/components/observe/CloudTelemetryActivationSection'
import { problemDetail } from '@/lib/api-problem'
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
import { Skeleton } from '@/components/ui/skeleton'
import { formatBytes } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  CloudUpload,
  HardDrive,
} from 'lucide-react'
import { Link } from 'react-router'

function humanAge(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`
  if (seconds < 86_400) return `${(seconds / 3600).toFixed(1)}h`
  return `${(seconds / 86_400).toFixed(1)}d`
}

function Stat({
  label,
  value,
  hint,
}: {
  label: string
  value: string
  hint?: string
}) {
  return (
    <div className="rounded-lg border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-lg font-semibold tabular-nums">{value}</p>
      {hint && <p className="mt-0.5 text-xs text-muted-foreground">{hint}</p>}
    </div>
  )
}

export function CloudTelemetryWriteStatusCard() {
  const { data, isPending, isError, error, refetch } = useQuery({
    ...getCloudTelemetryStatusOptions(),
    refetchInterval: 30_000,
  })

  return (
    <Card>
      <CardHeader>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle className="flex items-center gap-2 text-base">
              <CloudUpload className="h-4 w-4" />
              Cloud telemetry writes
            </CardTitle>
            <CardDescription>
              Projects whose spans go straight to Temps Cloud instead of being
              stored here, and whether this instance still needs a span store.
            </CardDescription>
          </div>
          {data && (
            <Badge variant={data.configured ? 'default' : 'secondary'}>
              {data.cloud_primary_projects} Cloud-primary ·{' '}
              {data.local_mode_projects} local
            </Badge>
          )}
        </div>
      </CardHeader>

      <CardContent className="space-y-4">
        <WriteStatusBody
          data={data}
          isPending={isPending}
          isError={isError}
          error={error}
          onRetry={() => void refetch()}
        />

        {/* ADR-042 §11. Rendered in every branch above, including the failure
            one: this instance's own status query being unhappy says nothing
            about whether an activation is running, and hiding the control here
            is how an operator concludes the feature does not exist. */}
        <CloudTelemetryActivationSection
          configured={data?.configured ?? false}
          statusPending={isPending}
          reason={data?.reason ?? undefined}
          setupPath={data?.setup_path ?? undefined}
        />
      </CardContent>
    </Card>
  )
}

function WriteStatusBody({
  data,
  isPending,
  isError,
  error,
  onRetry,
}: {
  data: CloudTelemetryWriteStatusResponse | undefined
  isPending: boolean
  isError: boolean
  error: unknown
  onRetry: () => void
}) {
  if (isPending) {
    return (
      <div className="space-y-3">
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-24 w-full" />
      </div>
    )
  }

  if (isError || !data) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>Status unavailable</AlertTitle>
        <AlertDescription className="space-y-3">
          <p>
            {problemDetail(
              error,
              'This instance could not report its Cloud telemetry queue. Queue depth and gap windows are unknown — not zero.'
            )}
          </p>
          <Button size="sm" variant="outline" onClick={onRetry}>
            Try again
          </Button>
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-4">
      {/* The decommission answer, first, because it is what the operator
            came here to find out. */}
      {data.local_span_store_required ? (
        <Alert>
          <HardDrive className="h-4 w-4" />
          <AlertTitle>
            This instance still needs its local span store
          </AlertTitle>
          <AlertDescription>
            {data.local_span_store_reason ??
              'Spans are still being written to this instance.'}
            {data.local_history_until && (
              <>
                {' '}
                Local history is readable through{' '}
                {new Date(data.local_history_until).toLocaleDateString()}.
              </>
            )}
          </AlertDescription>
        </Alert>
      ) : (
        <Alert>
          <CheckCircle2 className="h-4 w-4" />
          <AlertTitle>
            No project writes spans to this instance any more
          </AlertTitle>
          <AlertDescription>
            Every project is Cloud-primary and no local span history remains
            inside retention, so a local span backend (ClickHouse, or the
            `otel_spans` hypertable) is no longer required for traces. Metrics,
            logs and every other signal still use local storage — only spans
            move.
          </AlertDescription>
        </Alert>
      )}

      {!data.configured && data.reason && (
        <div className="rounded-md border border-dashed bg-muted/40 p-4">
          <p className="text-sm font-medium">
            Cloud-primary telemetry writes are not set up
          </p>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">
            {data.reason}
          </p>
          {data.setup_path && (
            <Button
              asChild
              size="sm"
              variant="outline"
              className="mt-3 gap-1.5"
            >
              <Link to={data.setup_path}>
                Set this up
                <ArrowRight className="size-3.5" />
              </Link>
            </Button>
          )}
        </div>
      )}

      {data.write_suspension && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>
            Cloud-primary writes are suspended — spans are being stored here
          </AlertTitle>
          <AlertDescription>
            {data.write_suspension} Project settings are unchanged and resume
            automatically once Cloud accepts again.
          </AlertDescription>
        </Alert>
      )}

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <Stat
          label="Spans queued"
          value={data.queue_depth.toLocaleString()}
          hint={
            data.queue_depth > 0
              ? 'Durable on disk; survives a restart'
              : 'Everything has reached Cloud'
          }
        />
        <Stat
          label="Queue size"
          value={formatBytes(data.queue_bytes)}
          hint={
            data.queue_max_bytes > 0
              ? `of ${formatBytes(data.queue_max_bytes)} before spans are dropped`
              : undefined
          }
        />
        <Stat
          label="Oldest unshipped"
          value={
            // `undefined` is not zero: an empty queue has no oldest span, and
            // rendering "0s" would read as "everything is instant".
            data.oldest_unshipped_age_secs == null
              ? '—'
              : humanAge(data.oldest_unshipped_age_secs)
          }
          hint={
            data.oldest_unshipped_age_secs == null
              ? 'Queue is empty'
              : 'Time this span has waited for Cloud'
          }
        />
        <Stat
          label="Gave up"
          value={data.dead_lettered_rows.toLocaleString()}
          hint={
            data.dead_lettered_rows > 0
              ? 'Retries exhausted; never swept automatically'
              : 'No shipment has exhausted its retries'
          }
        />
      </div>

      {data.gap_windows.length > 0 && (
        <div className="space-y-2">
          <p className="text-xs font-medium text-destructive">
            Spans not captured anywhere (last 30 days)
          </p>
          {data.gap_windows.map((gap) => (
            <div
              key={`${gap.project_id}-${gap.started_at}`}
              className="rounded-md border border-destructive/40 bg-destructive/5 p-3 text-xs leading-5"
            >
              <p className="font-medium">
                Project {gap.project_id}: {gap.dropped_spans.toLocaleString()}{' '}
                span
                {gap.dropped_spans === 1 ? '' : 's'} (
                {formatBytes(gap.dropped_bytes)}) between{' '}
                {new Date(gap.started_at).toLocaleString()} and{' '}
                {new Date(gap.ended_at).toLocaleString()}
              </p>
              <p className="mt-1 text-muted-foreground">{gap.message}</p>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

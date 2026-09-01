// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * "Some of your spans are not here yet, and some are never coming" — shown
 * above the trace list for a Cloud-primary project (ADR-041 §9).
 *
 * A Cloud-primary project's traces are only as complete as the outbox is
 * drained. Without this banner a backlog looks exactly like an application that
 * stopped emitting spans, and a gap window looks exactly like a quiet hour.
 * Both are situations where a self-hosted operator has nobody to ask, so the
 * page has to volunteer the difference.
 *
 * Renders nothing when there is nothing to say: no backlog, no gap, no
 * fallback. It is deliberately not gated on a feature check — the query simply
 * comes back with zeros on a `local` project.
 */

import {
  fetchProjectCloudTelemetry,
  projectCloudTelemetryKey,
  type TelemetryGapWindow,
} from '@/api/cloudTelemetry'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { AlertTriangle, ArrowRight, CloudUpload } from 'lucide-react'
import { Link } from 'react-router'

interface TelemetryBacklogBannerProps {
  projectId: number
  projectSlug: string
  /**
   * ISO bounds of the window the list is showing, so only relevant gaps
   * surface. A gap from last month is noise on a "last hour" view.
   */
  startTime?: string
  endTime?: string
}

/** Gaps that overlap the window on screen. */
function gapsInWindow(
  gaps: TelemetryGapWindow[],
  startTime?: string,
  endTime?: string,
): TelemetryGapWindow[] {
  if (!startTime || !endTime) return gaps
  const from = new Date(startTime).getTime()
  const to = new Date(endTime).getTime()
  if (Number.isNaN(from) || Number.isNaN(to)) return gaps
  return gaps.filter((gap) => {
    const started = new Date(gap.started_at).getTime()
    const ended = new Date(gap.ended_at).getTime()
    return ended >= from && started <= to
  })
}

export function TelemetryBacklogBanner({
  projectId,
  projectSlug,
  startTime,
  endTime,
}: TelemetryBacklogBannerProps) {
  const { data } = useQuery({
    queryKey: projectCloudTelemetryKey(projectId),
    queryFn: () => fetchProjectCloudTelemetry(projectId),
    // A backlog drains; an operator watching one needs it to move without a
    // manual refresh.
    refetchInterval: 30_000,
  })

  if (!data) return null

  const gaps = gapsInWindow(data.gap_windows, startTime, endTime)
  const backlog = data.queued_spans
  const fallingBack = data.effective_write_mode !== data.write_mode

  if (backlog === 0 && gaps.length === 0 && !fallingBack) return null

  const droppedSpans = gaps.reduce((total, gap) => total + gap.dropped_spans, 0)
  const oldestGap = gaps.reduce<TelemetryGapWindow | null>(
    (oldest, gap) =>
      !oldest || new Date(gap.started_at) < new Date(oldest.started_at)
        ? gap
        : oldest,
    null,
  )

  return (
    <div className="space-y-3">
      {gaps.length > 0 && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>
            {droppedSpans.toLocaleString()} span
            {droppedSpans === 1 ? '' : 's'} in this time range were never
            captured
          </AlertTitle>
          <AlertDescription className="space-y-2">
            <p>
              {oldestGap?.message ??
                'This instance could not store or ship these spans, so they exist nowhere.'}{' '}
              {gaps.length > 1
                ? `${gaps.length} separate periods are affected.`
                : `The gap runs from ${new Date(gaps[0].started_at).toLocaleString()} to ${new Date(gaps[0].ended_at).toLocaleString()}.`}
            </p>
            <p className="text-xs">
              Traces below are complete apart from these periods — this is not a
              query problem and refreshing will not recover them.
            </p>
          </AlertDescription>
        </Alert>
      )}

      {fallingBack && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>
            This project’s spans are not going where its settings say
          </AlertTitle>
          <AlertDescription className="space-y-2">
            <p>
              {data.effective_reason_message ??
                'Temps Cloud is not accepting this project’s spans, so they are being stored on this instance instead.'}{' '}
              Traces from this period are read from this instance, not from
              Cloud.
            </p>
            <Button asChild size="sm" variant="outline" className="gap-1.5">
              <Link to={`/projects/${projectSlug}/settings/telemetry`}>
                Telemetry storage
                <ArrowRight className="size-3.5" />
              </Link>
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {backlog > 0 && (
        <Alert>
          <CloudUpload className="h-4 w-4" />
          <AlertTitle>
            {backlog.toLocaleString()} span{backlog === 1 ? '' : 's'} have not
            reached Temps Cloud yet
          </AlertTitle>
          <AlertDescription>
            They are durably queued on this instance and will appear here once
            Cloud accepts them — a restart will not lose them. Recent traces may
            look incomplete until the queue drains.
            {data.intervals[0]?.effective_from && (
              <>
                {' '}
                This project has been Cloud-primary{' '}
                {formatDistanceToNow(new Date(data.intervals[0].effective_from), {
                  addSuffix: true,
                })}
                .
              </>
            )}
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

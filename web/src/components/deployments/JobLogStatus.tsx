// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  AlertTriangle,
  Clock,
  FileX,
  RefreshCw,
  Radio,
  WifiOff,
} from 'lucide-react'
import {
  type JobLogBody,
  type JobLogNotice,
  MAX_VIEWER_LINES,
} from './job-log-state'

/**
 * Shown when the lines on screen do not start at the beginning of the log:
 * the viewer only loads the most recent {@link MAX_VIEWER_LINES} lines, and
 * keeps at most that many as the stream grows. Log lines are numbered from 1,
 * so a first line above 1 means earlier lines exist but were not loaded.
 */
export function JobLogTruncationNote({
  firstLine,
  missingLines = 0,
}: {
  firstLine: number | undefined
  missingLines?: number
}) {
  const truncated = firstLine !== undefined && firstLine > 1
  if (!truncated && missingLines <= 0) return null
  return (
    <div role="status" className="space-y-1 text-xs text-muted-foreground">
      {truncated ? (
        <p>
          Showing the most recent lines, from line{' '}
          {firstLine.toLocaleString('en-US')}. Up to{' '}
          {MAX_VIEWER_LINES.toLocaleString('en-US')} lines are kept in the
          browser.
        </p>
      ) : null}
      {missingLines > 0 ? (
        <p>
          {missingLines.toLocaleString('en-US')} lines are missing between the
          displayed sections. Live output exceeded the fallback polling window.
          These lines are not included in the displayed log.
        </p>
      ) : null}
    </div>
  )
}

const SKELETON_WIDTHS = ['w-3/4', 'w-1/2', 'w-2/3', 'w-5/12', 'w-7/12']

interface JobLogPlaceholderProps {
  body: Exclude<JobLogBody, 'lines'>
  jobStatus: string
  detail: string | null
  onRetry: () => void
}

/** Content of the log pane when there are no lines to show. */
export function JobLogPlaceholder({
  body,
  jobStatus,
  detail,
  onRetry,
}: JobLogPlaceholderProps) {
  switch (body) {
    case 'loading':
      return (
        <div className="space-y-2" aria-label="Loading logs" role="status">
          {SKELETON_WIDTHS.map((width) => (
            <Skeleton key={width} className={`h-3 ${width}`} />
          ))}
        </div>
      )
    case 'connecting':
      return (
        <PlaceholderMessage icon={Radio}>
          Connecting to the live log stream…
        </PlaceholderMessage>
      )
    case 'waiting-for-output':
      return (
        <PlaceholderMessage icon={Radio}>
          Connected. Waiting for log output…
        </PlaceholderMessage>
      )
    case 'polling-for-output':
      return (
        <PlaceholderMessage icon={Clock}>
          No log output yet. Checking again every few seconds…
        </PlaceholderMessage>
      )
    case 'not-started':
      return (
        <PlaceholderMessage icon={Clock}>
          This stage hasn&apos;t started yet. Its logs will appear here once it
          runs.
        </PlaceholderMessage>
      )
    case 'no-output':
      return (
        <PlaceholderMessage icon={FileX}>
          {jobStatus === 'cancelled' || jobStatus === 'skipped'
            ? `This stage was ${jobStatus} before it wrote any log output.`
            : 'This stage finished without writing any log output.'}
        </PlaceholderMessage>
      )
    case 'gone':
      return (
        <PlaceholderMessage icon={FileX} detail={detail}>
          The logs for this stage are no longer available.
        </PlaceholderMessage>
      )
    case 'error':
      return (
        <PlaceholderMessage
          icon={AlertTriangle}
          tone="error"
          detail={detail}
          action={
            <Button
              variant="outline"
              size="sm"
              onClick={onRetry}
              className="font-sans"
            >
              <RefreshCw className="h-3.5 w-3.5" />
              Retry
            </Button>
          }
        >
          Couldn&apos;t load the logs for this stage.
        </PlaceholderMessage>
      )
  }
}

interface PlaceholderMessageProps {
  icon: typeof Clock
  children: React.ReactNode
  detail?: string | null
  tone?: 'muted' | 'error'
  action?: React.ReactNode
}

function PlaceholderMessage({
  icon: Icon,
  children,
  detail,
  tone = 'muted',
  action,
}: PlaceholderMessageProps) {
  return (
    <div role="status" className="flex flex-col items-start gap-2 font-sans">
      <p
        className={`flex items-center gap-2 text-sm ${
          tone === 'error' ? 'text-destructive' : 'text-muted-foreground'
        }`}
      >
        <Icon className="h-4 w-4 shrink-0" aria-hidden="true" />
        {children}
      </p>
      {detail ? (
        <p className="whitespace-pre-wrap text-xs text-muted-foreground">
          {detail}
        </p>
      ) : null}
      {action}
    </div>
  )
}

interface JobLogNoticeBarProps {
  notice: JobLogNotice
  onRetry: () => void
}

/** Transport status above the pane while lines are being shown. */
export function JobLogNoticeBar({ notice, onRetry }: JobLogNoticeBarProps) {
  if (notice === 'none') return null
  return (
    <div
      role="status"
      className="flex flex-wrap items-center gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-800 dark:text-amber-200"
    >
      <NoticeContent notice={notice} />
      {notice === 'refresh-failed' ? (
        <Button
          variant="outline"
          size="sm"
          className="ml-auto h-7"
          onClick={onRetry}
        >
          <RefreshCw className="h-3.5 w-3.5" />
          Retry
        </Button>
      ) : null}
    </div>
  )
}

function NoticeContent({ notice }: { notice: Exclude<JobLogNotice, 'none'> }) {
  switch (notice) {
    case 'connecting':
      return (
        <>
          <Radio className="h-4 w-4 shrink-0" aria-hidden="true" />
          Connecting to the live log stream…
        </>
      )
    case 'polling':
      return (
        <>
          <WifiOff className="h-4 w-4 shrink-0" aria-hidden="true" />
          Live stream unavailable. Refreshing logs every few seconds while Temps
          reconnects.
        </>
      )
    case 'partial-log':
      return (
        <>
          <FileX className="h-4 w-4 shrink-0" aria-hidden="true" />
          The complete log for this stage is no longer available. Showing only
          the lines received while it ran.
        </>
      )
    case 'refresh-failed':
      return (
        <>
          <AlertTriangle className="h-4 w-4 shrink-0" aria-hidden="true" />
          Couldn&apos;t refresh the logs. Showing the lines already received.
        </>
      )
  }
}

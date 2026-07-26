import {
  listBackupAlertsOptions,
  type BackupAlertResponse,
} from '@/lib/backup-alerts'
import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNowStrict } from 'date-fns'
import { Bell, CalendarClock, ChevronRight, Loader2 } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { Button } from '../ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '../ui/popover'
import { Separator } from '../ui/separator'

/**
 * Global header button that surfaces open backup alerts (overdue schedules
 * + stalled jobs). Visible on every page so operators see problems even when
 * they're not on `/backups`. Renders the bell icon with a count badge when
 * alerts exist; a popover lists each alert.
 *
 * Color coding — any open alert is a danger state. Backups not firing is
 * never "informational" — it means a recovery point is missing. The badge
 * and bell are both `destructive` whenever `count > 0`. We don't grade
 * severity in the chrome; the row content carries the detail.
 *
 * The watcher auto-resolves alerts, so the badge clears itself when the
 * underlying condition lifts. The list polls every 60s via `listBackupAlertsOptions`.
 */
export function BackupAlertsButton() {
  const { data, isLoading } = useQuery(listBackupAlertsOptions())
  const alerts = data?.alerts ?? []
  const count = alerts.length
  const hasAlerts = count > 0
  const [open, setOpen] = useState(false)

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          size="icon"
          className={
            'relative ' +
            (hasAlerts
              ? 'border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive'
              : '')
          }
          aria-label={
            count === 0
              ? 'Backup alerts (no alerts)'
              : `Backup alerts (${count} open)`
          }
        >
          <Bell className="size-4" />
          {hasAlerts && (
            <span
              className="absolute -right-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[10px] font-semibold leading-none text-destructive-foreground tabular-nums ring-2 ring-background"
              aria-hidden="true"
            >
              {count > 9 ? '9+' : count}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" sideOffset={6} className="w-[360px] p-0">
        <div className="flex items-center justify-between px-3 py-2.5">
          <p className="text-sm font-medium">Backup alerts</p>
          {count > 0 && (
            <p className="text-xs text-muted-foreground tabular-nums">
              {count} open
            </p>
          )}
        </div>
        <Separator />
        {isLoading ? (
          <div className="flex items-center justify-center px-3 py-8">
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
          </div>
        ) : count === 0 ? (
          <EmptyState />
        ) : (
          <ul
            role="list"
            className="max-h-[420px] divide-y divide-border overflow-y-auto"
          >
            {alerts.map((alert) => (
              <li key={alert.id}>
                <AlertRow alert={alert} onNavigate={() => setOpen(false)} />
              </li>
            ))}
          </ul>
        )}
      </PopoverContent>
    </Popover>
  )
}

/**
 * Build the deep-link target for an alert. Returns `null` when there isn't
 * enough information to link (e.g. the schedule or backup has been deleted
 * while the alert was open).
 */
function alertHref(alert: BackupAlertResponse): string | null {
  if (alert.kind === 'overdue_schedule' && alert.schedule_s3_source_id !== null) {
    // Schedules are listed on the S3 source detail page that owns them.
    return `/backups/s3-sources/${alert.schedule_s3_source_id}`
  }
  if (
    alert.kind === 'stalled_job' &&
    alert.backup_id !== null &&
    alert.backup_s3_source_id !== null
  ) {
    return `/backups/s3-sources/${alert.backup_s3_source_id}/backups/${alert.backup_id}`
  }
  return null
}

function EmptyState() {
  return (
    <div className="px-3 py-8 text-center">
      <p className="text-sm text-muted-foreground">All backups are healthy.</p>
    </div>
  )
}

function AlertRow({
  alert,
  onNavigate,
}: {
  alert: BackupAlertResponse
  onNavigate: () => void
}) {
  const kindLabel =
    alert.kind === 'overdue_schedule' ? 'Schedule overdue' : 'Job stalled'
  const openedAgo = formatDistanceToNowStrict(new Date(alert.opened_at), {
    addSuffix: true,
  })

  const targetLabel =
    alert.kind === 'overdue_schedule'
      ? alert.schedule_name ??
        (alert.schedule_id !== null ? `Schedule #${alert.schedule_id}` : 'Unknown schedule')
      : alert.job_id !== null
        ? `Job #${alert.job_id}`
        : 'Unknown job'

  const href = alertHref(alert)

  // Single content block used inside either the <Link> (clickable variant) or
  // a static <div> fallback (when we don't have enough info to deep-link).
  const body = (
    <>
      <span
        className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-destructive/10 text-destructive"
        aria-hidden="true"
      >
        <CalendarClock className="size-4" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2">
          <p className="truncate text-sm">
            <span className="mr-1.5 font-medium text-destructive">
              {kindLabel}
            </span>
            <span className="text-muted-foreground">·</span>{' '}
            <span className="font-medium text-foreground">{targetLabel}</span>
          </p>
          <p className="shrink-0 text-xs text-muted-foreground tabular-nums">
            {openedAgo}
          </p>
        </div>
        <p className="mt-0.5 line-clamp-2 text-xs text-muted-foreground">
          {alert.message}
        </p>
      </div>
      {href !== null && (
        <ChevronRight
          className="mt-1 size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
      )}
    </>
  )

  if (href === null) {
    return (
      <div className="flex items-start gap-3 px-3 py-3">{body}</div>
    )
  }

  return (
    <Link
      to={href}
      onClick={onNavigate}
      className="flex items-start gap-3 px-3 py-3 transition-colors hover:bg-accent focus-visible:bg-accent focus-visible:outline-none"
    >
      {body}
    </Link>
  )
}

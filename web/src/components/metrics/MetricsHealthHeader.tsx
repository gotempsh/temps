// Datadog-style triaged status surface, pinned above the metrics tabs.
//
// Inverts the page: the system surfaces what's wrong (firing alerts + anomalies,
// worst-first) before any raw chart. Honest about coverage — distinguishes
// "all healthy" from "nothing is being watched yet" so a quiet project never
// shows false-green reassurance.

import { ProjectResponse } from '@/api/client'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ruleStatus,
  STATUS_META,
  useAlertStatus,
  type AlertStatusLevel,
} from '@/components/metrics/alert-status'
import { alertSummary, StatusDot } from '@/components/metrics/alert-format'
import { BellOff, CheckCircle2, ChevronRight } from 'lucide-react'
import { Link, useLocation } from 'react-router-dom'

const COUNT_ORDER: AlertStatusLevel[] = ['alert', 'warn', 'nodata', 'ok']

export function MetricsHealthHeader({ project }: { project: ProjectResponse }) {
  const status = useAlertStatus(project.id)

  // The hub mounts at `…/metrics/*`; derive that base for absolute alert links.
  const { pathname } = useLocation()
  const i = pathname.indexOf('/metrics')
  const base = i === -1 ? pathname : pathname.slice(0, i + '/metrics'.length)

  if (status.isLoading) {
    // Single fixed-height band — never N rows — so it can't shift layout.
    return <Skeleton className="h-12 w-full rounded-lg" />
  }

  // Never imply health when we couldn't read the status.
  if (status.isError) {
    return (
      <div className="rounded-lg border border-border bg-card px-3 py-2.5 text-sm text-muted-foreground">
        Couldn&apos;t load alert status.
      </div>
    )
  }

  // No rules → honest "not watching anything" coverage state, not false-green.
  if (!status.hasRules) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-dashed border-border bg-card px-3 py-2.5 text-sm">
        <BellOff className="size-4 shrink-0 text-muted-foreground" />
        <span className="text-muted-foreground">
          Nothing is being watched yet.{' '}
          <Link
            to={`${base}/alerts/new`}
            className="font-medium text-foreground underline-offset-2 hover:underline"
          >
            Create an alert
          </Link>{' '}
          to get notified when a metric goes wrong.
        </span>
      </div>
    )
  }

  const { counts, firing, gathering } = status
  const allClear = firing.length === 0

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-card">
      {/* Summary bar */}
      <div className="flex flex-col gap-2 px-3 py-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
          {allClear ? (
            <span className="flex items-center gap-1.5 text-sm font-medium text-emerald-600 dark:text-emerald-400">
              <CheckCircle2 className="size-4" />
              All systems healthy
            </span>
          ) : (
            <span className="text-sm font-semibold">Needs attention</span>
          )}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            {COUNT_ORDER.map((lvl) =>
              counts[lvl] > 0 ? (
                <span key={lvl} className="inline-flex items-center gap-1.5">
                  <StatusDot level={lvl} />
                  <span className="tabular-nums">{counts[lvl]}</span>{' '}
                  {STATUS_META[lvl].label}
                </span>
              ) : null,
            )}
          </div>
        </div>
        {allClear && gathering.length > 0 && (
          <span className="text-xs text-amber-600 dark:text-amber-400">
            {gathering.length} anomaly rule{gathering.length === 1 ? '' : 's'}{' '}
            gathering baseline
          </span>
        )}
      </div>

      {/* Firing rows — worst first. Only shown when something is wrong. */}
      {firing.length > 0 && (
        <ul className="divide-y divide-border border-t border-border">
          {firing.map((rule) => (
            <li key={rule.id}>
              <Link
                to={`${base}/alerts/${rule.id}/edit`}
                className="flex items-center gap-2.5 px-3 py-2 text-sm transition-colors hover:bg-muted/50"
              >
                <StatusDot level={ruleStatus(rule)} pulse />
                <span className="flex min-w-0 flex-1 flex-col sm:flex-row sm:items-baseline sm:gap-2">
                  <span className="truncate font-medium">{rule.name}</span>
                  <span className="truncate font-mono text-xs text-muted-foreground">
                    {alertSummary(rule)}
                  </span>
                </span>
                <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

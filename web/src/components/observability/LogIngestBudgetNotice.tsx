// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Callout } from '@temps-sdk/ds'
import { Link } from 'react-router'
import type {
  LogIngestBudgetStatus,
  ThrottledLogSource,
} from '@/api/client/types.gen'
import { useLogIngestBudget } from '@/hooks/useGlobalLogs'

/** How many throttled containers are itemized; the title counts them all. */
const LISTED = 5

const number = new Intl.NumberFormat()

function formatRate(bytesPerSec: number): string {
  const units = ['B/s', 'KiB/s', 'MiB/s', 'GiB/s']
  let value = bytesPerSec
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`
}

function describeBudget(
  budget: Pick<LogIngestBudgetStatus, 'lines_per_sec' | 'bytes_per_sec'>
): string {
  const parts = [
    budget.lines_per_sec > 0
      ? `${number.format(budget.lines_per_sec)} lines/s`
      : null,
    budget.bytes_per_sec > 0 ? formatRate(budget.bytes_per_sec) : null,
  ].filter(Boolean)
  return parts.length > 0 ? parts.join(' and ') : 'no limit'
}

function sourceName(
  source: ThrottledLogSource,
  projectLabels: Record<string, string>
): string {
  const owner =
    source.project_id != null
      ? (projectLabels[String(source.project_id)] ??
        `project #${source.project_id}`)
      : 'database service'
  const node = source.node_name ? ` on ${source.node_name}` : ''
  return `${source.service} (${owner})${node}`
}

export type LogIngestScope =
  | { projectId: number; externalServiceId?: undefined }
  | { externalServiceId: number; projectId?: undefined }

function inScope(
  source: ThrottledLogSource,
  scope: LogIngestScope | undefined
): boolean {
  if (!scope) return true
  if (scope.externalServiceId != null) {
    return source.external_service_id === scope.externalServiceId
  }
  return (
    source.external_service_id == null && source.project_id === scope.projectId
  )
}

/**
 * Names every container that is logging faster than its ingestion budget.
 * Their excess lines are dropped rather than stored, so without this the
 * operator sees gaps in a noisy service's history and nothing explaining
 * them. Renders nothing when no container is over budget.
 */
export function LogIngestBudgetNotice({
  budget,
  projectLabels = {},
  scope,
  className,
}: {
  budget: LogIngestBudgetStatus | undefined
  /** Project names by id, as the log explorer resolves them. */
  projectLabels?: Record<string, string>
  /** Only containers of this project or external service. */
  scope?: LogIngestScope
  className?: string
}) {
  const throttled = (budget?.throttled ?? []).filter((source) =>
    inScope(source, scope)
  )
  if (!budget || throttled.length === 0) return null
  const count = throttled.length
  const hidden = count - LISTED
  return (
    <Callout
      tone="warning"
      className={className}
      title={
        count === 1
          ? '1 container is logging faster than its ingestion budget'
          : `${count} containers are logging faster than their ingestion budget`
      }
    >
      <p>
        Each container may store {describeBudget(budget)} (bursts of up to{' '}
        {budget.burst_seconds}s pass). Lines over that are dropped, not stored,
        and each gap is marked by a notice in the container&apos;s own log.
      </p>
      <ul className="mt-2 space-y-1">
        {throttled.slice(0, LISTED).map((source) => (
          <li key={source.container_id} className="text-xs">
            <span className="font-medium text-foreground">
              {sourceName(source, projectLabels)}
            </span>{' '}
            <code className="text-[11px]">
              {source.container_id.slice(0, 12)}
            </code>
            {' · '}
            {source.offered_lines_per_sec > 0
              ? `logging ~${number.format(source.offered_lines_per_sec)} lines/s (${formatRate(source.offered_bytes_per_sec)})`
              : 'quiet now'}
            {' · '}
            {number.format(source.dropped_lines)} lines dropped since{' '}
            {new Date(source.throttled_since).toLocaleTimeString()}
            {source.limit_overridden
              ? ` · own budget: ${describeBudget({
                  lines_per_sec: source.lines_per_sec_limit,
                  bytes_per_sec: source.bytes_per_sec_limit,
                })}`
              : null}
          </li>
        ))}
      </ul>
      {hidden > 0 ? <p className="mt-1 text-xs">and {hidden} more.</p> : null}
      <p className="mt-2 text-xs">
        Make the container log less, or{' '}
        <Link
          to={budget.setup_path}
          className="underline underline-offset-2 hover:text-foreground"
        >
          change the ingestion budget in Settings
        </Link>
        . A project or managed service with its own budget changes it in its own
        settings.
      </p>
    </Callout>
  )
}

/**
 * The notice with its data: polls the ingestion budget status for a log
 * view, optionally narrowed to one project or external service.
 */
export function LogIngestBudgetWarning({
  scope,
  label,
  projectLabels,
  className,
}: {
  scope?: LogIngestScope
  /** How to name the scoped project in each line. */
  label?: string
  /** Project names by id, for an unscoped view. */
  projectLabels?: Record<string, string>
  className?: string
}) {
  const status = useLogIngestBudget()
  return (
    <LogIngestBudgetNotice
      budget={status.data}
      scope={scope}
      className={className}
      projectLabels={
        label && scope?.projectId != null
          ? { [String(scope.projectId)]: label }
          : projectLabels
      }
    />
  )
}

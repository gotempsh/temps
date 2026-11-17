// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getShieldSummaryOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  formatSolveMs,
  SHIELD_WINDOWS,
  shieldModeDescription,
  shieldModeEnforces,
  shieldModeLabel,
  type ShieldWindowValue,
} from '@/lib/shield'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { ShieldSection } from './ShieldSection'

const numberFormat = new Intl.NumberFormat()

/**
 * The summary for one window; every part of the tab shares one request.
 * While another window loads, the previous one stays as placeholder data:
 * the mode (whose sentence names its own window) keeps it, the counts do not.
 */
function useShieldSummary(
  path: ShieldEnvironmentPath,
  timeWindow: ShieldWindowValue
) {
  return useQuery({
    ...getShieldSummaryOptions({ path, query: { window: timeWindow } }),
    placeholderData: keepPreviousData,
  })
}

function windowLabel(timeWindow: string) {
  return (
    SHIELD_WINDOWS.find((option) => option.value === timeWindow)?.label ??
    timeWindow
  )
}

/** Picks the window the Activity group counts over. */
export function ShieldWindowSelect({
  value,
  onChange,
}: {
  value: ShieldWindowValue
  onChange: (value: ShieldWindowValue) => void
}) {
  return (
    <Select
      value={value}
      onValueChange={(next) => onChange(next as ShieldWindowValue)}
    >
      <SelectTrigger className="w-full sm:w-[170px]" aria-label="Time window">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {SHIELD_WINDOWS.map((option) => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

/**
 * The first thing on the tab: what Shield does with its verdicts here, and,
 * while it only records them, what enforcing would have done. A plain line,
 * not an alert: logging only is a deliberate choice, not a problem.
 */
export function ShieldModeStatus({
  path,
  timeWindow,
  onOpenSettings,
}: {
  path: ShieldEnvironmentPath
  timeWindow: ShieldWindowValue
  onOpenSettings?: () => void
}) {
  const query = useShieldSummary(path, timeWindow)
  const summary = query.data
  return (
    <section
      aria-labelledby="shield-mode-heading"
      className="flex flex-col gap-3 border-b pb-6 sm:flex-row sm:items-start sm:justify-between"
    >
      {!summary && query.isError ? (
        <div className="min-w-0 space-y-1">
          <h2 id="shield-mode-heading" className="text-lg font-semibold">
            Mode unknown
          </h2>
          <p className="text-sm text-muted-foreground">
            Shield&apos;s summary for this environment did not load, so its mode
            is not known.{' '}
            <button
              type="button"
              className="underline underline-offset-4"
              disabled={query.isFetching}
              onClick={() => void query.refetch()}
            >
              {query.isFetching ? 'Retrying…' : 'Retry'}
            </button>
          </p>
        </div>
      ) : summary ? (
        <div className="min-w-0 space-y-1">
          <h2 id="shield-mode-heading" className="text-lg font-semibold">
            Mode: {shieldModeLabel(summary.mode)}{' '}
            <span className="text-sm font-normal text-muted-foreground">
              {summary.mode_source === 'environment'
                ? 'set on this environment'
                : 'inherited from the service'}
            </span>
          </h2>
          <p className="text-sm text-muted-foreground">
            {shieldModeDescription(summary.mode)}
          </p>
          {!shieldModeEnforces(summary.mode) && (
            <p className="text-sm text-muted-foreground">
              Selective mode would have blocked{' '}
              <span className="font-medium text-foreground tabular-nums">
                {numberFormat.format(summary.would_have.blocked)}
              </span>{' '}
              and challenged{' '}
              <span className="font-medium text-foreground tabular-nums">
                {numberFormat.format(summary.would_have.challenged)}
              </span>{' '}
              {summary.would_have.challenged === 1 ? 'request' : 'requests'} in
              the {windowLabel(summary.window).toLowerCase()}.
            </p>
          )}
        </div>
      ) : (
        <div
          role="status"
          aria-label="Loading Shield mode"
          className="space-y-2"
        >
          <h2 id="shield-mode-heading" className="sr-only">
            Mode
          </h2>
          <Skeleton className="h-6 w-56" />
          <Skeleton className="h-4 w-80 max-w-full" />
        </div>
      )}
      {onOpenSettings && (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="shrink-0"
          onClick={onOpenSettings}
        >
          Change mode
        </Button>
      )}
    </section>
  )
}

/** Verdict and challenge counts for the window, grouped by what they count. */
export function ShieldVerdictStats({
  path,
  timeWindow,
}: {
  path: ShieldEnvironmentPath
  timeWindow: ShieldWindowValue
}) {
  const summary = useShieldSummary(path, timeWindow)
  // The previous window's counts would read as this window's.
  if (!summary.data || summary.isPlaceholderData)
    return summary.error ? (
      <ReadFailure
        resource="Shield summary"
        error={summary.error}
        onRetry={() => summary.refetch()}
        retrying={summary.isFetching}
        embedded
      />
    ) : (
      <StatsSkeleton />
    )
  const { verdicts, challenges, solve_times: solveTimes } = summary.data
  return (
    <div className="space-y-4">
      {summary.error != null && (
        <ReadFailure
          resource="Shield summary"
          error={summary.error}
          cached
          onRetry={() => summary.refetch()}
          embedded
        />
      )}
      <div className="grid gap-6 md:grid-cols-2">
        <StatGroup title="Verdicts">
          <Stat label="Block" value={verdicts.block} />
          <Stat label="Challenge" value={verdicts.challenge} />
          <Stat label="Log" value={verdicts.log} />
        </StatGroup>
        <StatGroup
          title="Challenges"
          note={
            solveTimes.samples === 0 ? (
              'No challenge was solved in this window.'
            ) : (
              <>
                Solved in{' '}
                <span className="font-medium text-foreground tabular-nums">
                  {formatSolveMs(solveTimes.median_ms)}
                </span>{' '}
                (median),{' '}
                <span className="font-medium text-foreground tabular-nums">
                  {formatSolveMs(solveTimes.p90_ms)}
                </span>{' '}
                (90th percentile).
              </>
            )
          }
        >
          <Stat label="Issued" value={challenges.issued} />
          <Stat label="Solved" value={challenges.solved} />
          <Stat label="Failed" value={challenges.failed} />
        </StatGroup>
      </div>
    </div>
  )
}

/** Requests that matched each Shield source list in the window. */
export function ShieldSources({
  path,
  timeWindow,
}: {
  path: ShieldEnvironmentPath
  timeWindow: ShieldWindowValue
}) {
  // A failed read is reported once, by the counts above.
  const summary = useShieldSummary(path, timeWindow)
  const sources = summary.isPlaceholderData ? undefined : summary.data?.sources
  return (
    <ShieldSection
      id="shield-sources"
      title="Requests by source"
      description="Requests that matched a Shield source list in this window."
    >
      {sources === undefined ? (
        <div role="status" aria-label="Loading sources" className="p-4">
          <Skeleton className="h-8 w-full" />
        </div>
      ) : sources.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">
          No request matched a Shield source in this window.
        </p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Source</TableHead>
              <TableHead className="text-right">Requests</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {sources.map((source) => (
              <TableRow key={source.source}>
                <TableCell className="font-mono text-xs">
                  {source.source}
                </TableCell>
                <TableCell className="text-right tabular-nums">
                  {numberFormat.format(source.requests)}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </ShieldSection>
  )
}

function StatGroup({
  title,
  note,
  children,
}: {
  title: string
  note?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="min-w-0 space-y-3">
      <h3 className="text-sm font-medium">{title}</h3>
      <dl className="grid grid-cols-3 gap-4">{children}</dl>
      {note && <p className="text-sm text-muted-foreground">{note}</p>}
    </div>
  )
}

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 text-2xl font-semibold tabular-nums">
        {numberFormat.format(value)}
      </dd>
    </div>
  )
}

function StatsSkeleton() {
  return (
    <div
      role="status"
      aria-label="Loading Shield summary"
      className="grid gap-6 md:grid-cols-2"
    >
      {[0, 1].map((group) => (
        <div key={group} className="space-y-3">
          <Skeleton className="h-4 w-24" />
          <div className="grid grid-cols-3 gap-4">
            {[0, 1, 2].map((item) => (
              <div key={item} className="space-y-2">
                <Skeleton className="h-3 w-16" />
                <Skeleton className="h-7 w-12" />
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getShieldSummaryOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ShieldSummaryResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { InfoIcon } from 'lucide-react'
import { useState } from 'react'
import type { ShieldEnvironmentPath } from './EnvironmentShieldView'
import { ShieldSection } from './ShieldSection'

const numberFormat = new Intl.NumberFormat()

export function ShieldSummaryPanel({
  path,
  onOpenSettings,
}: {
  path: ShieldEnvironmentPath
  onOpenSettings?: () => void
}) {
  const [timeWindow, setTimeWindow] = useState<ShieldWindowValue>('24h')
  const summary = useQuery({
    ...getShieldSummaryOptions({ path, query: { window: timeWindow } }),
    placeholderData: keepPreviousData,
  })

  return (
    <ShieldSection
      id="shield-summary"
      title="Shield"
      description="What Shield decided for this environment's requests, and what it does with those verdicts."
      actions={
        <Select
          value={timeWindow}
          onValueChange={(value) => setTimeWindow(value as ShieldWindowValue)}
        >
          <SelectTrigger className="w-[170px]" aria-label="Time window">
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
      }
    >
      {summary.data ? (
        <SummaryContent
          summary={summary.data}
          refreshError={summary.error}
          onRetry={() => summary.refetch()}
          onOpenSettings={onOpenSettings}
        />
      ) : summary.error ? (
        <div className="p-4">
          <ReadFailure
            resource="Shield summary"
            error={summary.error}
            onRetry={() => summary.refetch()}
            retrying={summary.isFetching}
            embedded
          />
        </div>
      ) : (
        <SummarySkeleton />
      )}
    </ShieldSection>
  )
}

function SummaryContent({
  summary,
  refreshError,
  onRetry,
  onOpenSettings,
}: {
  summary: ShieldSummaryResponse
  refreshError: unknown
  onRetry: () => void
  onOpenSettings?: () => void
}) {
  const enforcing = shieldModeEnforces(summary.mode)
  const windowLabel =
    SHIELD_WINDOWS.find((option) => option.value === summary.window)?.label ??
    summary.window
  return (
    <div className="divide-y">
      {refreshError != null && (
        <div className="p-4">
          <ReadFailure
            resource="Shield summary"
            error={refreshError}
            cached
            onRetry={onRetry}
            embedded
          />
        </div>
      )}
      <div className="flex flex-wrap items-start justify-between gap-3 p-4">
        <div className="min-w-0 space-y-1">
          <p className="text-sm">
            <span className="font-medium">
              Mode: {shieldModeLabel(summary.mode)}
            </span>{' '}
            <span className="text-muted-foreground">
              {summary.mode_source === 'environment'
                ? '(set on this environment)'
                : `(inherited from the service)`}
            </span>
          </p>
          <p className="text-sm text-muted-foreground">
            {shieldModeDescription(summary.mode)}
          </p>
        </div>
        {onOpenSettings && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onOpenSettings}
          >
            Change mode
          </Button>
        )}
      </div>

      {!enforcing && (
        <div className="p-4">
          <Alert>
            <InfoIcon className="h-4 w-4" />
            <AlertTitle>Before you enforce</AlertTitle>
            <AlertDescription>
              In selective mode,{' '}
              {numberFormat.format(summary.would_have.blocked)}{' '}
              {summary.would_have.blocked === 1 ? 'request' : 'requests'} from
              the {windowLabel.toLowerCase()} would have been blocked and{' '}
              {numberFormat.format(summary.would_have.challenged)} challenged.
            </AlertDescription>
          </Alert>
        </div>
      )}

      <dl className="grid grid-cols-2 gap-4 p-4 sm:grid-cols-3 lg:grid-cols-6">
        <Stat label="Block verdicts" value={summary.verdicts.block} />
        <Stat label="Challenge verdicts" value={summary.verdicts.challenge} />
        <Stat label="Log verdicts" value={summary.verdicts.log} />
        <Stat label="Challenges issued" value={summary.challenges.issued} />
        <Stat label="Challenges solved" value={summary.challenges.solved} />
        <Stat label="Challenges failed" value={summary.challenges.failed} />
      </dl>

      <div className="p-4">
        <h3 className="text-sm font-medium">Solve times</h3>
        {summary.solve_times.samples === 0 ? (
          <p className="mt-1 text-sm text-muted-foreground">
            No challenge was solved in this window.
          </p>
        ) : (
          <p className="mt-1 text-sm text-muted-foreground">
            Median{' '}
            <span className="font-medium text-foreground tabular-nums">
              {formatSolveMs(summary.solve_times.median_ms)}
            </span>
            , 90th percentile{' '}
            <span className="font-medium text-foreground tabular-nums">
              {formatSolveMs(summary.solve_times.p90_ms)}
            </span>{' '}
            over {numberFormat.format(summary.solve_times.samples)} solved{' '}
            {summary.solve_times.samples === 1 ? 'challenge' : 'challenges'}.
          </p>
        )}
      </div>

      <div className="p-4">
        <h3 className="text-sm font-medium">Requests by source</h3>
        {summary.sources.length === 0 ? (
          <p className="mt-1 text-sm text-muted-foreground">
            No request matched a Shield source in this window.
          </p>
        ) : (
          <Table className="mt-2">
            <TableHeader>
              <TableRow>
                <TableHead>Source</TableHead>
                <TableHead className="text-right">Requests</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {summary.sources.map((source) => (
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
      </div>
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

function SummarySkeleton() {
  return (
    <div role="status" aria-label="Loading Shield summary" className="divide-y">
      <div className="space-y-2 p-4">
        <Skeleton className="h-4 w-48" />
        <Skeleton className="h-4 w-80 max-w-full" />
      </div>
      <div className="grid grid-cols-2 gap-4 p-4 sm:grid-cols-3 lg:grid-cols-6">
        {[0, 1, 2, 3, 4, 5].map((item) => (
          <div key={item} className="space-y-2">
            <Skeleton className="h-3 w-24" />
            <Skeleton className="h-7 w-16" />
          </div>
        ))}
      </div>
      <div className="space-y-2 p-4">
        <Skeleton className="h-4 w-32" />
        <Skeleton className="h-8 w-full" />
      </div>
    </div>
  )
}

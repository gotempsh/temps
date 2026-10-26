// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironmentsOptions,
  getShieldSummaryOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { shieldModeLabel } from '@/lib/shield'
import { useQueries, useQuery } from '@tanstack/react-query'

const numberFormat = new Intl.NumberFormat()

/**
 * How many of the last 24 hours' requests each environment would have
 * blocked or challenged in selective mode (ADR 066 D11), so the switch is
 * made knowing its effect.
 */
export function ShieldWouldHaveCounts({ projectId }: { projectId: number }) {
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId } }),
  })
  const list = environments.data ?? []
  const summaries = useQueries({
    queries: list.map((environment) => ({
      ...getShieldSummaryOptions({
        path: { project_id: projectId, environment_id: environment.id },
        query: { window: '24h' },
      }),
    })),
  })

  return (
    <div className="space-y-2">
      <div>
        <h3 className="text-sm font-medium">Last 24 hours</h3>
        <p className="text-sm text-muted-foreground">
          Requests that would have been blocked or challenged in selective mode.
          Each environment&apos;s Shield tab has the detail.
        </p>
      </div>
      {environments.error ? (
        <ReadFailure
          resource="Environments"
          error={environments.error}
          onRetry={() => environments.refetch()}
          retrying={environments.isFetching}
          embedded
        />
      ) : environments.data === undefined ? (
        <div
          role="status"
          aria-label="Loading Shield counts"
          className="space-y-2"
        >
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </div>
      ) : list.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          This service has no environments yet.
        </p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Environment</TableHead>
              <TableHead>Mode</TableHead>
              <TableHead className="text-right">Would block</TableHead>
              <TableHead className="text-right">Would challenge</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {list.map((environment, index) => {
              const summary = summaries[index]
              return (
                <TableRow key={environment.id}>
                  <TableCell className="font-medium">
                    {environment.name}
                  </TableCell>
                  <TableCell className="text-sm text-muted-foreground">
                    {summary?.data
                      ? `${shieldModeLabel(summary.data.mode)}${summary.data.mode_source === 'project' ? ' (inherited)' : ''}`
                      : '—'}
                  </TableCell>
                  {summary?.data ? (
                    <>
                      <TableCell className="text-right tabular-nums">
                        {numberFormat.format(summary.data.would_have.blocked)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {numberFormat.format(
                          summary.data.would_have.challenged
                        )}
                      </TableCell>
                    </>
                  ) : summary?.error ? (
                    <TableCell
                      colSpan={2}
                      className="text-right text-sm text-destructive"
                    >
                      Counts unavailable
                    </TableCell>
                  ) : (
                    <TableCell colSpan={2} className="text-right">
                      <Skeleton className="ml-auto h-4 w-24" />
                    </TableCell>
                  )}
                </TableRow>
              )
            })}
          </TableBody>
        </Table>
      )}
    </div>
  )
}

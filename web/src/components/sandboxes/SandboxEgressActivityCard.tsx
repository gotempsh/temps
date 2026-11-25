// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The sandbox's own recorded outbound connections, newest first, read by
 * sandbox rather than through its project's logs. While recording is off
 * the card stays visible in an onboarding state showing what a record looks
 * like and where to turn it on.
 */
import { useQuery } from '@tanstack/react-query'
import { Activity } from 'lucide-react'
import { useState } from 'react'

import type {
  EgressActivityRecord,
  EgressOutcomeFilter,
  EgressRecordingStatus,
} from '@/api/client'
import {
  getEgressRecordingOptions,
  listEgressRecordsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
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
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import { formatBytes } from '@/lib/utils'

import {
  describeDestination,
  EXAMPLE_RECORDS,
  formatDuration,
  outcomeVariant,
} from './egress-recording'

const PAGE_SIZE = 20
/** How often the first page refreshes while recording is on. */
const REFRESH_MS = 10_000

type OutcomeChoice = 'all' | EgressOutcomeFilter

export function SandboxEgressActivityCard({
  sandboxId,
  disabled = false,
}: {
  sandboxId: string
  /** Destroyed sandboxes still show what they recorded. */
  disabled?: boolean
}) {
  const [page, setPage] = useState(1)
  const [outcome, setOutcome] = useState<OutcomeChoice>('all')
  const status = useQuery(
    getEgressRecordingOptions({ path: { id: sandboxId } })
  )
  const recording = status.data?.enabled === true
  const records = useQuery({
    ...listEgressRecordsOptions({
      path: { id: sandboxId },
      query: {
        page,
        per_page: PAGE_SIZE,
        outcome: outcome === 'all' ? undefined : outcome,
      },
    }),
    refetchInterval: recording && page === 1 && !disabled ? REFRESH_MS : false,
  })
  const total = records.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  return (
    <Card>
      <CardHeader className="flex flex-col gap-2 pb-3 sm:flex-row sm:items-center sm:justify-between sm:space-y-0">
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Activity className="h-4 w-4" aria-hidden />
          Outbound activity
        </CardTitle>
        <Select
          value={outcome}
          onValueChange={(value) => {
            setOutcome(value as OutcomeChoice)
            setPage(1)
          }}
        >
          <SelectTrigger
            className="w-full sm:w-[160px]"
            aria-label="Filter by outcome"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All outcomes</SelectItem>
            <SelectItem value="allowed">Allowed</SelectItem>
            <SelectItem value="refused">Refused</SelectItem>
            <SelectItem value="failed">Failed</SelectItem>
          </SelectContent>
        </Select>
      </CardHeader>
      <CardContent className="@container space-y-3">
        {records.isError ? (
          <ErrorAlert
            title="Could not load outbound activity"
            description={problemDetail(
              records.error,
              "This sandbox's recorded connections could not be loaded."
            )}
            retry={() => records.refetch()}
            retrying={records.isFetching}
          />
        ) : records.data === undefined ? (
          <div className="space-y-2" aria-busy="true">
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
          </div>
        ) : records.data.records.length === 0 ? (
          <EmptyActivity
            status={status.data}
            filtered={outcome !== 'all'}
            disabled={disabled}
          />
        ) : (
          <>
            <ActivityTable records={records.data.records} />
            {totalPages > 1 && (
              <ResponsivePagination
                page={page}
                pageSize={PAGE_SIZE}
                total={total}
                totalPages={totalPages}
                onPageChange={setPage}
                ariaLabel="Outbound activity pagination"
              />
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

function EmptyActivity({
  status,
  filtered,
  disabled,
}: {
  status: EgressRecordingStatus | undefined
  filtered: boolean
  disabled: boolean
}) {
  if (filtered) {
    return (
      <p className="text-sm text-muted-foreground">
        No connections with this outcome.
      </p>
    )
  }
  if (status?.enabled || disabled) {
    return (
      <p className="text-sm text-muted-foreground">
        No connections recorded yet.
      </p>
    )
  }
  return (
    <div
      role="status"
      className="space-y-2 rounded-md border border-dashed p-3 text-sm"
    >
      <p className="font-medium">Recording is off</p>
      <p className="text-muted-foreground">
        Turn on “Record outbound connections” to list every connection this
        sandbox opens here, e.g.
      </p>
      <ul className="space-y-1 font-mono text-xs text-muted-foreground">
        {EXAMPLE_RECORDS.map((line) => (
          <li key={line} className="[overflow-wrap:anywhere]">
            {line}
          </li>
        ))}
      </ul>
    </div>
  )
}

function ActivityTable({ records }: { records: EgressActivityRecord[] }) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead className="hidden @xl:table-cell">Time</TableHead>
          <TableHead>Destination</TableHead>
          <TableHead>Outcome</TableHead>
          <TableHead className="hidden @2xl:table-cell">Transfer</TableHead>
          <TableHead className="hidden @2xl:table-cell">Duration</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {records.map((record, index) => (
          <ActivityRow key={`${record.timestamp}-${index}`} record={record} />
        ))}
      </TableBody>
    </Table>
  )
}

function ActivityRow({ record }: { record: EgressActivityRecord }) {
  const time = new Date(record.timestamp)
  const transfer = `${formatBytes(record.bytes_sent, 1)} sent, ${formatBytes(record.bytes_received, 1)} received`
  return (
    <TableRow>
      <TableCell className="hidden whitespace-nowrap text-xs text-muted-foreground @xl:table-cell">
        <time dateTime={record.timestamp} title={time.toISOString()}>
          {time.toLocaleString()}
        </time>
      </TableCell>
      <TableCell>
        <span className="font-mono text-xs [overflow-wrap:anywhere]">
          {describeDestination(record)}
        </span>
        <span className="block text-xs text-muted-foreground @xl:hidden">
          {time.toLocaleString()}
        </span>
        <span className="block text-xs text-muted-foreground @2xl:hidden">
          {transfer} · {formatDuration(record.duration_ms)}
        </span>
      </TableCell>
      <TableCell>
        <Badge variant={outcomeVariant(record.outcome)}>{record.outcome}</Badge>
        {record.count > 1 && (
          <span className="ml-1 text-xs text-muted-foreground">
            ×{record.count}
          </span>
        )}
        {record.reason && (
          <span className="block text-xs text-muted-foreground [overflow-wrap:anywhere]">
            {record.reason}
          </span>
        )}
      </TableCell>
      <TableCell className="hidden text-xs @2xl:table-cell">
        {transfer}
      </TableCell>
      <TableCell className="hidden text-xs @2xl:table-cell">
        {formatDuration(record.duration_ms)}
      </TableCell>
    </TableRow>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getPageFlowOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  DropOffPoint,
  PageFlowEntry,
  PageTransition,
} from '@/api/client/types.gen'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { SortableTableHead } from '@/components/ui/sortable-table-head'
import {
  nextTableSort,
  sortTableRows,
  type TableSort,
} from '@/lib/client-table-sort'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import {
  ArrowRight,
  DoorOpen,
  LogIn,
  TrendingDown,
  Loader2,
  AlertCircle,
} from 'lucide-react'
import { type PageFlowProps, durationSortValue } from './PageFlow-shared'

function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null || seconds <= 0) return '-'
  if (seconds < 60) return `${Math.round(seconds)}s`
  const mins = Math.floor(seconds / 60)
  const secs = Math.round(seconds % 60)
  return secs > 0 ? `${mins}m ${secs}s` : `${mins}m`
}

function formatRate(rate: number): string {
  return `${(rate * 100).toFixed(1)}%`
}

function RateBar({ rate, color }: { rate: number; color: string }) {
  return (
    <div className="flex items-center gap-2">
      <div className="w-16 h-2 bg-muted rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full ${color}`}
          style={{ width: `${Math.min(rate * 100, 100)}%` }}
        />
      </div>
      <span className="text-xs text-muted-foreground tabular-nums">
        {formatRate(rate)}
      </span>
    </div>
  )
}

function EntryPagesTable({ pages }: { pages: PageFlowEntry[] }) {
  type SortKey = 'page' | 'entries' | 'views' | 'bounce' | 'time'
  const [sort, setSort] = useState<TableSort<SortKey>>({
    key: 'entries',
    direction: 'desc',
  })
  const sortedPages = useMemo(
    () =>
      sortTableRows(
        pages,
        (page) =>
          ({
            page: page.page_path,
            entries: page.entry_count,
            views: page.total_views,
            bounce: page.bounce_rate,
            time: durationSortValue(page.avg_time_on_page),
          })[sort.key],
        sort.direction
      ),
    [pages, sort]
  )

  if (pages.length === 0) {
    return (
      <p className="text-sm text-muted-foreground py-4 text-center">
        No entry pages found in this period
      </p>
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <SortableTableHead
            label="Page"
            active={sort.key === 'page'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'page'))}
          />
          <SortableTableHead
            label="Entries"
            active={sort.key === 'entries'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'entries'))
            }
            align="right"
          />
          <SortableTableHead
            label="Views"
            active={sort.key === 'views'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'views'))
            }
            align="right"
          />
          <SortableTableHead
            label="Bounce Rate"
            active={sort.key === 'bounce'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'bounce'))
            }
          />
          <SortableTableHead
            label="Avg. Time"
            active={sort.key === 'time'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'time'))}
          />
        </TableRow>
      </TableHeader>
      <TableBody>
        {sortedPages.map((page) => (
          <TableRow key={page.page_path}>
            <TableCell className="font-mono text-sm max-w-[300px] truncate">
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="cursor-default">{page.page_path}</span>
                  </TooltipTrigger>
                  <TooltipContent>{page.page_path}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            </TableCell>
            <TableCell className="text-right tabular-nums font-medium">
              {page.entry_count.toLocaleString()}
            </TableCell>
            <TableCell className="text-right tabular-nums text-muted-foreground">
              {page.total_views.toLocaleString()}
            </TableCell>
            <TableCell>
              <RateBar rate={page.bounce_rate} color="bg-orange-500" />
            </TableCell>
            <TableCell className="text-muted-foreground text-sm tabular-nums">
              {formatDuration(page.avg_time_on_page)}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function ExitPagesTable({ pages }: { pages: PageFlowEntry[] }) {
  type SortKey = 'page' | 'exits' | 'views' | 'rate' | 'time'
  const [sort, setSort] = useState<TableSort<SortKey>>({
    key: 'exits',
    direction: 'desc',
  })
  const sortedPages = useMemo(
    () =>
      sortTableRows(
        pages,
        (page) =>
          ({
            page: page.page_path,
            exits: page.exit_count,
            views: page.total_views,
            rate: page.exit_rate,
            time: durationSortValue(page.avg_time_on_page),
          })[sort.key],
        sort.direction
      ),
    [pages, sort]
  )

  if (pages.length === 0) {
    return (
      <p className="text-sm text-muted-foreground py-4 text-center">
        No exit pages found in this period
      </p>
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <SortableTableHead
            label="Page"
            active={sort.key === 'page'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'page'))}
          />
          <SortableTableHead
            label="Exits"
            active={sort.key === 'exits'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'exits'))
            }
            align="right"
          />
          <SortableTableHead
            label="Views"
            active={sort.key === 'views'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'views'))
            }
            align="right"
          />
          <SortableTableHead
            label="Exit Rate"
            active={sort.key === 'rate'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'rate'))}
          />
          <SortableTableHead
            label="Avg. Time"
            active={sort.key === 'time'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'time'))}
          />
        </TableRow>
      </TableHeader>
      <TableBody>
        {sortedPages.map((page) => (
          <TableRow key={page.page_path}>
            <TableCell className="font-mono text-sm max-w-[300px] truncate">
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="cursor-default">{page.page_path}</span>
                  </TooltipTrigger>
                  <TooltipContent>{page.page_path}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            </TableCell>
            <TableCell className="text-right tabular-nums font-medium">
              {page.exit_count.toLocaleString()}
            </TableCell>
            <TableCell className="text-right tabular-nums text-muted-foreground">
              {page.total_views.toLocaleString()}
            </TableCell>
            <TableCell>
              <RateBar rate={page.exit_rate} color="bg-red-500" />
            </TableCell>
            <TableCell className="text-muted-foreground text-sm tabular-nums">
              {formatDuration(page.avg_time_on_page)}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function DropOffTable({ points }: { points: DropOffPoint[] }) {
  type SortKey = 'page' | 'exits' | 'views' | 'rate'
  const [sort, setSort] = useState<TableSort<SortKey>>({
    key: 'rate',
    direction: 'desc',
  })
  const sortedPoints = useMemo(
    () =>
      sortTableRows(
        points,
        (point) =>
          ({
            page: point.page_path,
            exits: point.exit_count,
            views: point.total_views,
            rate: point.exit_rate,
          })[sort.key],
        sort.direction
      ),
    [points, sort]
  )

  if (points.length === 0) {
    return (
      <p className="text-sm text-muted-foreground py-4 text-center">
        No significant drop-off points found
      </p>
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <SortableTableHead
            label="Page"
            active={sort.key === 'page'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'page'))}
          />
          <SortableTableHead
            label="Exits"
            active={sort.key === 'exits'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'exits'))
            }
            align="right"
          />
          <SortableTableHead
            label="Views"
            active={sort.key === 'views'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'views'))
            }
            align="right"
          />
          <SortableTableHead
            label="Exit Rate"
            active={sort.key === 'rate'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'rate'))}
          />
        </TableRow>
      </TableHeader>
      <TableBody>
        {sortedPoints.map((point) => (
          <TableRow key={point.page_path}>
            <TableCell className="font-mono text-sm max-w-[300px] truncate">
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="cursor-default">{point.page_path}</span>
                  </TooltipTrigger>
                  <TooltipContent>{point.page_path}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            </TableCell>
            <TableCell className="text-right tabular-nums font-medium">
              {point.exit_count.toLocaleString()}
            </TableCell>
            <TableCell className="text-right tabular-nums text-muted-foreground">
              {point.total_views.toLocaleString()}
            </TableCell>
            <TableCell>
              <RateBar rate={point.exit_rate} color="bg-red-600" />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function TransitionsTable({ transitions }: { transitions: PageTransition[] }) {
  type SortKey = 'from' | 'to' | 'count' | 'percentage'
  const [sort, setSort] = useState<TableSort<SortKey>>({
    key: 'count',
    direction: 'desc',
  })
  const sortedTransitions = useMemo(
    () =>
      sortTableRows(
        transitions,
        (transition) =>
          ({
            from: transition.from_page,
            to: transition.to_page,
            count: transition.transition_count,
            percentage: transition.percentage,
          })[sort.key],
        sort.direction
      ),
    [sort, transitions]
  )

  if (transitions.length === 0) {
    return (
      <p className="text-sm text-muted-foreground py-4 text-center">
        No page transitions found in this period
      </p>
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <SortableTableHead
            label="From"
            active={sort.key === 'from'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'from'))}
          />
          <TableHead className="w-8" />
          <SortableTableHead
            label="To"
            active={sort.key === 'to'}
            direction={sort.direction}
            onClick={() => setSort((current) => nextTableSort(current, 'to'))}
          />
          <SortableTableHead
            label="Count"
            active={sort.key === 'count'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'count'))
            }
            align="right"
          />
          <SortableTableHead
            label="% of Source"
            active={sort.key === 'percentage'}
            direction={sort.direction}
            onClick={() =>
              setSort((current) => nextTableSort(current, 'percentage'))
            }
            align="right"
          />
        </TableRow>
      </TableHeader>
      <TableBody>
        {sortedTransitions.map((t, idx) => (
          <TableRow key={`${t.from_page}-${t.to_page}-${idx}`}>
            <TableCell className="font-mono text-sm max-w-[200px] truncate">
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="cursor-default">{t.from_page}</span>
                  </TooltipTrigger>
                  <TooltipContent>{t.from_page}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            </TableCell>
            <TableCell className="text-center">
              <ArrowRight className="h-4 w-4 text-muted-foreground" />
            </TableCell>
            <TableCell className="font-mono text-sm max-w-[200px] truncate">
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="cursor-default">{t.to_page}</span>
                  </TooltipTrigger>
                  <TooltipContent>{t.to_page}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            </TableCell>
            <TableCell className="text-right tabular-nums font-medium">
              {t.transition_count.toLocaleString()}
            </TableCell>
            <TableCell className="text-right tabular-nums text-muted-foreground">
              {t.percentage.toFixed(1)}%
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

export function PageFlow({
  project,
  startDate,
  endDate,
  environment,
}: PageFlowProps) {
  const { data, isLoading, isError } = useQuery({
    ...getPageFlowOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (isError || !data) {
    return (
      <div className="flex flex-col items-center justify-center py-12 gap-2">
        <AlertCircle className="h-8 w-8 text-destructive" />
        <p className="text-sm text-muted-foreground">
          Failed to load page flow analytics
        </p>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Summary Stats */}
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
        <Card>
          <CardHeader className="pb-2">
            <CardDescription>Total Pages</CardDescription>
            <CardTitle className="text-2xl tabular-nums">
              {data.total_pages.toLocaleString()}
            </CardTitle>
          </CardHeader>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardDescription>Total Sessions</CardDescription>
            <CardTitle className="text-2xl tabular-nums">
              {data.total_sessions.toLocaleString()}
            </CardTitle>
          </CardHeader>
        </Card>
      </div>

      {/* Entry Pages */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <LogIn className="h-5 w-5 text-green-500" />
            <div>
              <CardTitle>Entry Pages</CardTitle>
              <CardDescription>
                Where visitors land when they first arrive
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <EntryPagesTable pages={data.top_entry_pages} />
        </CardContent>
      </Card>

      {/* Exit Pages */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <DoorOpen className="h-5 w-5 text-red-500" />
            <div>
              <CardTitle>Exit Pages</CardTitle>
              <CardDescription>Where visitors leave your site</CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <ExitPagesTable pages={data.top_exit_pages} />
        </CardContent>
      </Card>

      {/* Drop-off Points */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <TrendingDown className="h-5 w-5 text-orange-500" />
            <div>
              <CardTitle>Drop-off Points</CardTitle>
              <CardDescription>
                Pages with the highest exit rates (minimum 5 views)
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <DropOffTable points={data.drop_off_points} />
        </CardContent>
      </Card>

      {/* Page Transitions */}
      <Card>
        <CardHeader>
          <div className="flex items-center gap-2">
            <ArrowRight className="h-5 w-5 text-blue-500" />
            <div>
              <CardTitle>Page Transitions</CardTitle>
              <CardDescription>
                Most common navigation paths between pages
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <TransitionsTable transitions={data.transitions} />
        </CardContent>
      </Card>
    </div>
  )
}

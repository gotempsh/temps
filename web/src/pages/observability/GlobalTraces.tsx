// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { number } from '@/lib/global-observability'
import { useGlobalView } from '@/hooks/useGlobalView'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { queryGlobalTraceSummariesOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  GlobalPage,
  GlobalPagination,
  QueryContent,
  FilterSelect,
} from '@/components/observability/GlobalPage'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Badge } from '@/components/ui/badge'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { OBSERVABILITY_PAGE_SIZE } from '@/lib/global-observability'

export default function GlobalTraces() {
  const view = useGlobalView()
  const status = view.params.get('status') === 'ERROR' ? 'ERROR' : 'all'
  const sort =
    view.params.get('sort') === 'duration' ? 'duration' : 'start_time'
  const query = useQuery({
    ...queryGlobalTraceSummariesOptions({
      query: {
        project_id: view.projectId,
        start_time: view.from,
        end_time: view.to,
        name_pattern: view.search || undefined,
        status: status === 'all' ? undefined : status,
        sort_by: sort,
        sort_order: 'desc',
        limit: OBSERVABILITY_PAGE_SIZE,
        offset: (view.page - 1) * OBSERVABILITY_PAGE_SIZE,
      },
    }),
    retry: false,
  })
  return (
    <GlobalPage
      title="Traces"
      description="Follow requests across services and projects."
      view={view}
      fetching={query.isFetching}
      refresh={() => void query.refetch()}
      searchLabel="Search span names"
      filters={
        <>
          <FilterSelect
            label="Trace status"
            value={status}
            onChange={(status) =>
              view.patch({ status: status === 'all' ? undefined : status })
            }
            options={[
              ['all', 'All statuses'],
              ['ERROR', 'Errors only'],
            ]}
          />
          <FilterSelect
            label="Sort traces"
            value={sort}
            onChange={(sort) => view.patch({ sort })}
            options={[
              ['start_time', 'Newest first'],
              ['duration', 'Slowest first'],
            ]}
          />
        </>
      }
    >
      <QueryContent
        title="Traces"
        loading={query.isPending}
        error={query.error}
        empty={!query.data?.data.length}
        retry={() => void query.refetch()}
      >
        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Trace</TableHead>
                <TableHead className="hidden md:table-cell">Project</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="text-right">Duration</TableHead>
                <TableHead className="hidden md:table-cell text-right">
                  Spans
                </TableHead>
                <TableHead className="hidden md:table-cell">Started</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {query.data?.data.map((trace) => (
                <TableRow key={`${trace.project_id}:${trace.trace_id}`}>
                  <TableCell className="min-w-36">
                    <p className="text-xs text-muted-foreground md:hidden">
                      {trace.project_name}
                    </p>
                    <Link
                      className="font-medium hover:underline"
                      to={`/projects/${encodeURIComponent(trace.project_slug)}/traces/${trace.trace_id}`}
                    >
                      {trace.root_span_name}
                    </Link>
                    <p className="text-xs text-muted-foreground">
                      {trace.service_name} ·{' '}
                      <span className="font-mono">
                        {trace.trace_id.slice(0, 8)}
                      </span>
                    </p>
                    <Link
                      className="text-xs text-muted-foreground underline whitespace-nowrap"
                      to={`/traces/global/${trace.trace_id}`}
                    >
                      Cross-project waterfall
                    </Link>
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    {trace.project_name}
                  </TableCell>
                  <TableCell>
                    <Badge
                      className="whitespace-nowrap"
                      variant={
                        trace.error_count > 0 ? 'destructive' : 'secondary'
                      }
                    >
                      {trace.error_count > 0
                        ? `${trace.error_count} ${trace.error_count === 1 ? 'error' : 'errors'}`
                        : trace.status_code === 'OK'
                          ? 'OK'
                          : 'Unset'}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {number(trace.duration_ms)} ms
                  </TableCell>
                  <TableCell className="hidden md:table-cell text-right tabular-nums">
                    {number(trace.span_count)}
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <TimeAgo date={trace.start_time} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
        <GlobalPagination view={view} total={query.data?.total ?? 0} />
      </QueryContent>
    </GlobalPage>
  )
}

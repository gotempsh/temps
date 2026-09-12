// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ArrowDown, ArrowUp, ArrowUpDown, Layers } from 'lucide-react'
import { ProjectCardMedia } from '@/components/dashboard/ProjectCardMedia'
import { useLatestDeploymentMedia } from '@/hooks/useLatestDeploymentMedia'
import { formatTraceDuration } from '@/lib/trace-presentation'
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
  const order = view.params.get('order') === 'asc' ? 'asc' : 'desc'
  const toggleDuration = () =>
    view.patch({
      sort: 'duration',
      order: sort === 'duration' && order === 'desc' ? 'asc' : 'desc',
    })
  const query = useQuery({
    ...queryGlobalTraceSummariesOptions({
      query: {
        project_id: view.projectId,
        start_time: view.from,
        end_time: view.to,
        name_pattern: view.search || undefined,
        status: status === 'all' ? undefined : status,
        sort_by: sort,
        sort_order: order,
        limit: OBSERVABILITY_PAGE_SIZE,
        offset: (view.page - 1) * OBSERVABILITY_PAGE_SIZE,
      },
    }),
    retry: false,
  })
  const projectIds = [
    ...new Set(query.data?.data.map((trace) => trace.project_id) ?? []),
  ].sort((a, b) => a - b)
  const media = useLatestDeploymentMedia(projectIds)
  const projectImage = (id: number, name: string) => (
    <ProjectCardMedia
      name={name}
      className="size-6 [&_img]:p-0.5"
      deploymentUrl={media.data?.projects?.[String(id)]?.url}
      screenshotLocation={
        media.data?.projects?.[String(id)]?.screenshot_location
      }
    />
  )
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
            value={`${sort}:${order}`}
            onChange={(value) => {
              const [sort, order] = value.split(':')
              view.patch({ sort, order })
            }}
            options={[
              ['start_time:desc', 'Newest first'],
              ['start_time:asc', 'Oldest first'],
              ['duration:desc', 'Slowest first'],
              ['duration:asc', 'Fastest first'],
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
                <TableHead
                  className="text-right"
                  aria-sort={
                    sort === 'duration'
                      ? order === 'desc'
                        ? 'descending'
                        : 'ascending'
                      : 'none'
                  }
                >
                  <button
                    type="button"
                    onClick={toggleDuration}
                    className="inline-flex items-center gap-1 rounded py-2 hover:text-foreground focus-visible:outline focus-visible:outline-ring"
                    aria-label={`Sort by duration, ${sort === 'duration' && order === 'desc' ? 'fastest' : 'slowest'} first`}
                  >
                    Duration
                    {sort !== 'duration' ? (
                      <ArrowUpDown className="size-3.5" />
                    ) : order === 'desc' ? (
                      <ArrowDown className="size-3.5" />
                    ) : (
                      <ArrowUp className="size-3.5" />
                    )}
                  </button>
                </TableHead>
                <TableHead className="hidden md:table-cell">Started</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {query.data?.data.map((trace) => (
                <TableRow key={`${trace.project_id}:${trace.trace_id}`}>
                  <TableCell className="min-w-36">
                    <p className="flex items-center gap-2 text-xs text-muted-foreground md:hidden">
                      {projectImage(trace.project_id, trace.project_name)}
                      {trace.project_name}
                    </p>
                    <Link
                      className="font-medium hover:underline"
                      to={`/projects/${encodeURIComponent(trace.project_slug)}/traces/${trace.trace_id}`}
                    >
                      {trace.root_span_name}
                    </Link>
                    <Badge
                      variant="secondary"
                      className="ml-2 gap-1 whitespace-nowrap font-normal"
                    >
                      <Layers className="size-3" />
                      {trace.span_count}{' '}
                      {trace.span_count === 1 ? 'span' : 'spans'}
                    </Badge>
                    <p className="text-xs text-muted-foreground">
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
                    <Link
                      to={`/projects/${encodeURIComponent(trace.project_slug)}`}
                      className="inline-flex items-center gap-2 hover:underline"
                    >
                      {projectImage(trace.project_id, trace.project_name)}
                      {trace.project_name}
                    </Link>
                  </TableCell>
                  <TableCell>
                    <Badge
                      className="whitespace-nowrap"
                      title={
                        trace.status_code === 'UNSET'
                          ? 'The instrumentation did not explicitly set a status. This is the OpenTelemetry default and does not indicate an error.'
                          : undefined
                      }
                      variant={
                        trace.error_count > 0 || trace.status_code === 'ERROR'
                          ? 'destructive'
                          : 'secondary'
                      }
                    >
                      {trace.error_count > 0
                        ? `${trace.error_count} ${trace.error_count === 1 ? 'error' : 'errors'}`
                        : trace.status_code === 'ERROR'
                          ? 'Error'
                          : trace.status_code === 'OK'
                            ? 'OK'
                            : 'Not reported'}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {formatTraceDuration(trace.duration_ms)}
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

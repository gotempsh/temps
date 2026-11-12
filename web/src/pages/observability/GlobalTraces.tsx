// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState, type ReactNode } from 'react'
import { traceDetailPath } from '@/lib/traces-time-window'
import { ChartGantt, Layers } from 'lucide-react'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import {
  durationBarClass,
  durationShare,
  formatTraceDuration,
  traceOutcome,
} from '@/lib/trace-presentation'
import { cn } from '@/lib/utils'
import type { GlobalTraceSummary } from '@/api/client/types.gen'
import { useGlobalView } from '@/hooks/useGlobalView'
import { useDebounce } from '@/hooks/useDebounce'
import { useAttributeFilter } from '@/hooks/useAttributeFilter'
import {
  type AttributeUrlFilter,
  canonicalAttributeFilter,
  planAttributeUrlSync,
} from '@/lib/attribute-facets'
import { RecordLink, RecordRow } from '@temps-sdk/ds'
import { Link } from 'react-router'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import {
  queryGlobalTraceSummariesOptions,
  queryGlobalTraceSummariesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { queryKeyPrefix } from '@/lib/refresh'
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
import { SortableTableHead } from '@/components/ui/sortable-table-head'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { OBSERVABILITY_PAGE_SIZE } from '@/lib/global-observability'

export default function GlobalTraces() {
  const view = useGlobalView()
  const status = view.params.get('status') === 'ERROR' ? 'ERROR' : 'all'
  const sort =
    view.params.get('sort') === 'duration' ? 'duration' : 'start_time'
  const order = view.params.get('order') === 'asc' ? 'asc' : 'desc'
  const toggleSort = (field: 'start_time' | 'duration') =>
    view.patch({
      sort: field,
      order: sort === field && order === 'desc' ? 'asc' : 'desc',
    })
  // Attribute filter. Typing is local and debounced into the URL and the
  // request, so each keystroke neither refetches nor resets the page.
  const urlKey = view.params.get('attr_key') ?? ''
  const urlValue = view.params.get('attr_value') ?? ''
  const [attrKey, setAttrKey] = useState(urlKey)
  const [attrValue, setAttrValue] = useState(urlValue)
  const attribute = useAttributeFilter({
    attrKey,
    attrValue,
    facetedOnly: false,
    onKeyChange: setAttrKey,
    onValueChange: setAttrValue,
  })
  const attributes = useDebounce(attribute.query, 300)
  const debouncedKey = useDebounce(attrKey, 300)
  const debouncedValue = useDebounce(attrValue, 300)
  const lastSeenUrl = useRef<AttributeUrlFilter>({
    key: urlKey,
    value: urlValue,
  })
  const patchView = view.patch
  useEffect(() => {
    const url = { key: urlKey, value: urlValue }
    const sync = planAttributeUrlSync({
      url,
      lastSeenUrl: lastSeenUrl.current,
      typed: canonicalAttributeFilter(attrKey, attrValue),
      settled: canonicalAttributeFilter(debouncedKey, debouncedValue),
    })
    lastSeenUrl.current = url
    if (sync.action === 'adopt') {
      // The URL changed from outside (a link, back/forward): follow it.
      setAttrKey(sync.filter.key)
      setAttrValue(sync.filter.value)
    } else if (sync.action === 'write') {
      patchView({
        attr_key: sync.filter.key || undefined,
        attr_value: sync.filter.value || undefined,
      })
    }
  }, [
    attrKey,
    attrValue,
    debouncedKey,
    debouncedValue,
    urlKey,
    urlValue,
    patchView,
  ])
  const query = useQuery({
    placeholderData: keepPreviousData,
    ...queryGlobalTraceSummariesOptions({
      query: {
        project_id: view.projectId,
        start_time: view.from,
        end_time: view.to,
        name_pattern: view.search || undefined,
        attributes,
        status: status === 'all' ? undefined : status,
        sort_by: sort,
        sort_order: order,
        limit: OBSERVABILITY_PAGE_SIZE,
        offset: (view.page - 1) * OBSERVABILITY_PAGE_SIZE,
      },
    }),
    retry: false,
  })
  const projectImage = (id: number, name: string) => (
    <ServiceIcon service={{ id, name }} size="sm" />
  )
  const traces = query.data?.data ?? []
  // Bars compare traces on this page, so the slowest one fills its track.
  const slowestMs = Math.max(0, ...traces.map((trace) => trace.duration_ms))
  return (
    <GlobalPage
      title="Traces"
      description="Follow requests across services."
      view={view}
      refreshQueryKeys={[queryKeyPrefix(queryGlobalTraceSummariesQueryKey)]}
      searchLabel="Search span names"
      filterNotice={attribute.notice}
      filters={
        <>
          {attribute.controls}
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
        empty={!traces.length}
        retry={() => void query.refetch()}
      >
        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Trace</TableHead>
                <TableHead className="hidden md:table-cell">Service</TableHead>
                <SortableTableHead
                  label="Duration"
                  active={sort === 'duration'}
                  direction={order}
                  onClick={() => toggleSort('duration')}
                  align="right"
                />
                <SortableTableHead
                  label="Started"
                  active={sort === 'start_time'}
                  direction={order}
                  onClick={() => toggleSort('start_time')}
                  className="hidden md:table-cell"
                />
              </TableRow>
            </TableHeader>
            <TableBody>
              {traces.map((trace) => (
                <TraceRow
                  key={`${trace.project_id}:${trace.trace_id}`}
                  trace={trace}
                  slowestMs={slowestMs}
                  projectImage={projectImage(
                    trace.project_id,
                    trace.project_name
                  )}
                />
              ))}
            </TableBody>
          </Table>
        </div>
        <GlobalPagination view={view} total={query.data?.total ?? 0} />
      </QueryContent>
    </GlobalPage>
  )
}

const OUTCOME = {
  error: { dot: 'bg-destructive', label: 'Failed', title: 'Failed' },
  ok: { dot: 'bg-emerald-500', label: 'OK', title: 'OK' },
  unset: {
    dot: 'border border-muted-foreground/50',
    label: 'Status not reported',
    title:
      'Status not reported: the instrumentation set none. This is the OpenTelemetry default and does not indicate an error.',
  },
} as const

/**
 * One line per trace: a status dot, the name (a link stretched over the
 * row), spans and ID, then service, a latency bar and when it started.
 */
export function TraceRow({
  trace,
  slowestMs,
  projectImage,
}: {
  trace: GlobalTraceSummary
  slowestMs: number
  projectImage: ReactNode
}) {
  const outcome = OUTCOME[traceOutcome(trace)]
  const projectPath = `/projects/${encodeURIComponent(trace.project_slug)}`
  return (
    <RecordRow>
      <TableCell className="min-w-36 py-2.5">
        <p className="mb-1 flex items-center gap-2 text-xs text-muted-foreground md:hidden">
          {projectImage}
          {trace.project_name}
        </p>
        <div className="flex min-w-0 items-center gap-2">
          <span
            className={cn('size-2 shrink-0 rounded-full', outcome.dot)}
            title={outcome.title}
          >
            <span className="sr-only">{outcome.label}</span>
          </span>
          <RecordLink
            className="min-h-0 min-w-0 [&>span]:truncate [&>span]:[word-break:normal]"
            to={`${projectPath}/traces/${traceDetailPath(trace)}`}
          >
            {trace.root_span_name}
          </RecordLink>
          {trace.error_count > 0 && (
            <Badge
              variant="destructive"
              className="shrink-0 whitespace-nowrap px-1.5 py-0 text-[11px]"
            >
              {trace.error_count} {trace.error_count === 1 ? 'error' : 'errors'}
            </Badge>
          )}
          <span className="hidden shrink-0 items-center gap-1 whitespace-nowrap text-xs text-muted-foreground sm:inline-flex">
            <Layers className="size-3" aria-hidden="true" />
            {trace.span_count}
            <span className="sr-only">
              {trace.span_count === 1 ? 'span' : 'spans'}
            </span>
            <span aria-hidden="true">·</span>
            <span className="font-mono">{trace.trace_id.slice(0, 8)}</span>
          </span>
          <Link
            to={`/traces/global/${traceDetailPath(trace)}`}
            aria-label="Cross-service waterfall"
            title="Cross-service waterfall"
            className="ml-auto shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <ChartGantt className="size-4" aria-hidden="true" />
          </Link>
        </div>
      </TableCell>
      <TableCell className="hidden py-2.5 md:table-cell">
        <Link
          to={projectPath}
          className="inline-flex max-w-56 items-center gap-2 hover:underline"
        >
          {projectImage}
          <span className="truncate">{trace.project_name}</span>
        </Link>
      </TableCell>
      <TableCell className="py-2.5">
        <div className="flex items-center justify-end gap-2">
          <span className="font-mono text-sm tabular-nums">
            {formatTraceDuration(trace.duration_ms)}
          </span>
          <span
            className="hidden h-1.5 w-16 overflow-hidden rounded-full bg-muted sm:block"
            aria-hidden="true"
          >
            <span
              className={cn(
                'block h-full rounded-full',
                durationBarClass(trace.duration_ms)
              )}
              style={{
                width: `${durationShare(trace.duration_ms, slowestMs) * 100}%`,
              }}
            />
          </span>
        </div>
      </TableCell>
      <TableCell className="hidden whitespace-nowrap py-2.5 text-sm text-muted-foreground md:table-cell">
        <TimeAgo date={trace.start_time} />
      </TableCell>
    </RecordRow>
  )
}

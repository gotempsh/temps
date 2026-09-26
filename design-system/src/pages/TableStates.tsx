// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { AlertCircle, Clock, Search, X } from 'lucide-react'
import {
  Button,
  Callout,
  DataTable,
  PageContainer,
  PageHeader,
  PageState,
  ResponsivePagination,
  Status,
  TimeChart,
  TimeRangeFilter,
  fmtDateTime,
  fmtDate,
  fmtTime,
  fmtDuration,
  fmtNumber,
  fmtPercent,
  resolveTimeRange,
  useUrlState,
  type DataTableColumn,
} from '@temps-sdk/ds'
import {
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Skeleton,
} from '@temps-sdk/ui'
import { createExecutionFixtures, type CronExecutionFixture } from '../fixtures'
import { executionOverview } from './execution-overview'

const SCENARIOS = [
  {
    value: 'loaded',
    label: 'Runs available',
    note: 'The overview, chart and table use the same filters.',
  },
  {
    value: 'loading',
    label: 'First load',
    note: 'Keep the controls visible and use skeletons until the first response arrives.',
  },
  {
    value: 'empty',
    label: 'No runs yet',
    note: 'A successful response with no records has its own explanation.',
  },
  {
    value: 'failed',
    label: 'Request failed',
    note: 'Offer a retry. A failed request is never an empty collection.',
  },
  {
    value: 'stale',
    label: 'Refresh failed',
    note: 'Keep the last usable data visible and explain that it may be out of date.',
  },
] as const

const columns: DataTableColumn<CronExecutionFixture>[] = [
  { key: 'task', header: 'Task', render: (run) => <code>{run.path}</code> },
  {
    key: 'time',
    header: 'Started',
    render: (run) => fmtDateTime(run.executedAt),
  },
  {
    key: 'status',
    header: 'Result',
    render: (run) => (
      <Status
        tone={run.statusCode >= 200 && run.statusCode < 300 ? 'ok' : 'error'}
        label={
          run.statusCode >= 200 && run.statusCode < 300 ? 'Succeeded' : 'Failed'
        }
      />
    ),
  },
  {
    key: 'duration',
    header: 'Duration',
    render: (run) => fmtDuration(run.durationMs),
  },
  {
    key: 'details',
    header: 'Details',
    render: (run) => (
      <span className="whitespace-normal">
        HTTP {run.statusCode}
        {run.error ? ` · ${run.error}` : ''}
      </span>
    ),
  },
]

export default function TableStates() {
  const { get, patch } = useUrlState<
    'scenario' | 'query' | 'range' | 'result' | 'page'
  >()
  const [records] = useState(() => createExecutionFixtures(Date.now()))
  const scenario =
    SCENARIOS.find((item) => item.value === get('scenario')) ?? SCENARIOS[0]
  const query = get('query') ?? ''
  const range = get('range') ?? '24h'
  const result = ['succeeded', 'failed'].includes(get('result') ?? '')
    ? get('result')!
    : 'all'
  const window = resolveTimeRange(range)
  const overview = executionOverview(
    scenario.value === 'empty' ? [] : records,
    window.from,
    window.to,
    query,
    result,
  )
  const loading = scenario.value === 'loading'
  const pageCount = Math.max(1, Math.ceil(overview.rows.length / 10))
  const requestedPage = Number(get('page') ?? 1)
  const page = Number.isSafeInteger(requestedPage)
    ? Math.min(pageCount, Math.max(1, requestedPage))
    : 1
  const retry = () => patch({ scenario: 'loaded' })
  const clear = () =>
    patch({ query: null, result: null, range: null, page: null })
  const hasFilters = Boolean(query || result !== 'all' || range !== '24h')
  const metrics = [
    {
      label: 'Total executions',
      value: fmtNumber(overview.rows.length),
      description: 'In the selected range',
    },
    {
      label: 'Success rate',
      value:
        overview.successRate === null ? '—' : fmtPercent(overview.successRate),
      description: 'Runs with an HTTP 2xx response',
    },
    {
      label: 'Failed executions',
      value: fmtNumber(overview.failed),
      description: 'Runs without an HTTP 2xx response',
    },
    {
      label: 'P95 duration',
      value: overview.p95 === null ? '—' : fmtDuration(overview.p95),
      description: '95% of runs finish within this time',
    },
  ]

  return (
    <PageContainer>
      <PageHeader
        title="Execution overview"
        description="Monitor scheduled tasks, spot failures, and inspect individual runs."
        actions={
          <span className="rounded-md border px-2 py-1 text-xs text-muted-foreground">
            Sample data
          </span>
        }
      />

      <div className="space-y-3">
        <div
          role="group"
          aria-label="Execution filters"
          className="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between"
        >
          <div className="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center">
            <div className="relative w-full sm:w-64">
              <Search
                aria-hidden="true"
                className="pointer-events-none absolute left-3 top-2.5 size-4 text-muted-foreground"
              />
              <Input
                aria-label="Filter by task path"
                placeholder="Search task paths…"
                className="h-9 pl-9"
                value={query}
                onChange={(event) =>
                  patch({ query: event.target.value || null, page: null })
                }
              />
            </div>
            <Select
              value={result}
              onValueChange={(value) =>
                patch({ result: value === 'all' ? null : value, page: null })
              }
            >
              <SelectTrigger
                aria-label="Execution result"
                className="h-9 w-full sm:w-40"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All results</SelectItem>
                <SelectItem value="succeeded">Succeeded</SelectItem>
                <SelectItem value="failed">Failed</SelectItem>
              </SelectContent>
            </Select>
            {hasFilters && (
              <Button variant="ghost" size="sm" onClick={clear}>
                <X className="size-3.5" /> Reset filters
              </Button>
            )}
          </div>
          <TimeRangeFilter
            value={range}
            onChange={(value) => patch({ range: value, page: null })}
          />
        </div>
        <p className="text-xs text-muted-foreground">
          {fmtDateTime(window.from)} – {fmtDateTime(window.to)} · local time
        </p>
      </div>

      {scenario.value === 'stale' && (
        <Callout tone="error" title="Couldn't refresh execution history">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p>Showing the last loaded runs. New runs may be missing.</p>
            <Button variant="outline" size="sm" onClick={retry}>
              Retry refresh
            </Button>
          </div>
        </Callout>
      )}

      {scenario.value === 'failed' ? (
        <div className="rounded-lg border">
          <PageState
            variant="failed"
            size="compact"
            icon={AlertCircle}
            title="Couldn't load execution history"
            description="The request timed out. Retry to see the overview and recent runs."
            action={<Button onClick={retry}>Retry loading history</Button>}
          />
        </div>
      ) : (
        <>
          <dl
            aria-label="Execution summary"
            className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border bg-border lg:grid-cols-4"
          >
            {metrics.map((metric) => (
              <div key={metric.label} className="space-y-2 bg-card p-4 sm:p-5">
                <dt className="text-sm text-muted-foreground">
                  {metric.label}
                </dt>
                <dd className="text-2xl font-semibold tracking-tight tabular-nums">
                  {loading ? <Skeleton className="h-8 w-20" /> : metric.value}
                </dd>
                <p className="text-xs text-muted-foreground">
                  {metric.description}
                </p>
              </div>
            ))}
          </dl>
          <section
            aria-labelledby="execution-trend-title"
            className="min-w-0 rounded-lg border p-4 sm:p-5"
          >
            <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
              <div>
                <h2
                  id="execution-trend-title"
                  className="text-sm font-semibold"
                >
                  Execution activity
                </h2>
                <p className="mt-1 text-xs text-muted-foreground">
                  Runs per{' '}
                  {fmtDuration(
                    (Date.parse(window.to) - Date.parse(window.from)) / 24,
                  )}{' '}
                  interval
                </p>
              </div>
              <div className="flex gap-3">
                <Status tone="ok" variant="dot" label="Succeeded" />
                <Status tone="error" variant="dot" label="Failed" />
              </div>
            </div>
            {loading ? (
              <Skeleton className="h-56 w-full" />
            ) : (
              <TimeChart
                data={overview.trend}
                xKey="time"
                series={[
                  { dataKey: 'succeeded', label: 'Succeeded', tone: 'good' },
                  { dataKey: 'failed', label: 'Failed', tone: 'poor' },
                ]}
                height={224}
                allowDecimals={false}
                xTickFormatter={(value) =>
                  Date.parse(window.to) - Date.parse(window.from) <= 86400000
                    ? fmtTime(String(value))
                    : fmtDate(String(value))
                }
              />
            )}
          </section>
          <section
            aria-labelledby="execution-history-title"
            className="min-w-0 space-y-4"
          >
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h2
                id="execution-history-title"
                className="text-base font-semibold"
              >
                Recent executions
              </h2>
              <p className="text-sm text-muted-foreground">
                {loading
                  ? 'Loading runs…'
                  : `${fmtNumber(overview.rows.length)} matching runs`}
              </p>
            </div>
            {scenario.value === 'empty' ? (
              <div className="rounded-md border">
                <PageState
                  variant="empty"
                  size="compact"
                  icon={Clock}
                  title="No executions yet"
                  description="Runs will appear after the first scheduled task executes."
                />
              </div>
            ) : !loading && overview.rows.length === 0 ? (
              <div className="rounded-md border">
                <PageState
                  variant="empty"
                  size="compact"
                  icon={Search}
                  title="No matching executions"
                  description="No runs match this task, result, and time range. Reset the filters to see recent activity."
                  action={
                    <Button variant="outline" onClick={clear}>
                      Reset filters
                    </Button>
                  }
                />
              </div>
            ) : (
              <DataTable
                aria-label="Recent executions"
                columns={columns}
                rows={overview.rows.slice((page - 1) * 10, page * 10)}
                rowKey={(run) => run.id}
                isLoading={loading}
              />
            )}
            {!loading && overview.rows.length > 0 && (
              <ResponsivePagination
                page={page}
                pageSize={10}
                total={overview.rows.length}
                totalPages={pageCount}
                onPageChange={(next) => patch({ page: next })}
              />
            )}
          </section>
        </>
      )}

      <details className="rounded-lg border p-4 text-sm">
        <summary className="cursor-pointer font-medium">
          Example controls
        </summary>
        <div className="mt-4 flex flex-col gap-3 sm:flex-row sm:items-center">
          <Select
            value={scenario.value}
            onValueChange={(value) => patch({ scenario: value, page: null })}
          >
            <SelectTrigger
              aria-label="Preview a request state"
              className="w-full sm:w-48"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SCENARIOS.map((item) => (
                <SelectItem key={item.value} value={item.value}>
                  {item.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p className="text-sm text-muted-foreground">
            {scenario.note} All data is invented; no API is called.
          </p>
          {loading && (
            <Button variant="outline" onClick={retry}>
              Finish sample request
            </Button>
          )}
        </div>
      </details>
    </PageContainer>
  )
}

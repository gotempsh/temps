// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { DataSection } from '@/components/data-display/DataSection'
import { MetricSummary } from '@/components/data-display/MetricSummary'
import { RankedList } from '@/components/data-display/RankedList'
import {
  Tabs,
  ScrollableTabsList,
  TabsTrigger,
  TabsContent,
} from '@/components/ui/tabs'

import { number } from '@/lib/global-observability'
import { useGlobalView } from '@/hooks/useGlobalView'
import { useQuery } from '@tanstack/react-query'
import { getGlobalAnalyticsOptions } from '@/api/client/@tanstack/react-query.gen'
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
import { OBSERVABILITY_PAGE_SIZE } from '@/lib/global-observability'
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'

export default function GlobalAnalytics() {
  const view = useGlobalView()
  const sort = ['views', 'visitors', 'sessions'].includes(
    view.params.get('sort') ?? ''
  )
    ? view.params.get('sort')!
    : 'views'
  const common = {
    project_id: view.projectId,
    start_date: view.from,
    end_date: view.to,
  }
  const query = useQuery({
    ...getGlobalAnalyticsOptions({
      query: {
        ...common,
        facet: 'pages',
        search: view.search || undefined,
        sort_by: sort,
        sort_order: 'desc',
        page: view.page,
        per_page: OBSERVABILITY_PAGE_SIZE,
      },
    }),
    retry: false,
  })
  const traffic = useQuery({
    ...getGlobalAnalyticsOptions({ query: { ...common, facet: 'traffic' } }),
    retry: false,
  })
  const summary = useQuery({
    ...getGlobalAnalyticsOptions({ query: { ...common, facet: 'summary' } }),
    retry: false,
  })
  const totals = summary.data?.rows[0]
  const tableView = view.params.get('chart') === 'table'
  return (
    <GlobalPage
      title="Analytics"
      description="Compare website traffic across your projects."
      view={view}
      fetching={query.isFetching || traffic.isFetching || summary.isFetching}
      refresh={() => {
        void query.refetch()
        void traffic.refetch()
        void summary.refetch()
      }}
      searchLabel="Search pages or projects"
      filters={
        <FilterSelect
          label="Sort analytics"
          value={sort}
          onChange={(sort) => view.patch({ sort })}
          options={[
            ['views', 'Most views'],
            ['visitors', 'Most visitors'],
            ['sessions', 'Most sessions'],
          ]}
        />
      }
    >
      <QueryContent
        title="Traffic summary"
        loading={summary.isPending}
        error={summary.error}
        empty={false}
        retry={() => void summary.refetch()}
      >
        <MetricSummary
          metrics={[
            { label: 'Page views', value: totals?.views ?? 0 },
            { label: 'Visitors', value: totals?.visitors ?? 0 },
            { label: 'Sessions', value: totals?.sessions ?? 0 },
          ]}
        />
      </QueryContent>
      <Tabs
        value={tableView ? 'table' : 'chart'}
        onValueChange={(value) =>
          view.patch({ chart: value === 'table' ? 'table' : undefined })
        }
      >
        <DataSection
          title="Page views over time"
          description="Traffic for the selected scope and time range."
          actions={
            <ScrollableTabsList aria-label="Traffic presentation">
              <TabsTrigger value="chart">Chart</TabsTrigger>
              <TabsTrigger value="table">Data table</TabsTrigger>
            </ScrollableTabsList>
          }
        >
          <QueryContent
            title="Traffic"
            loading={traffic.isPending}
            error={traffic.error}
            empty={!traffic.data?.rows.length}
            retry={() => void traffic.refetch()}
          >
            <TabsContent value={tableView ? 'table' : 'chart'} className="mt-0">
              {tableView ? (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Hour (UTC)</TableHead>
                      <TableHead className="text-right">Views</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {traffic.data?.rows.map((row) => (
                      <TableRow key={row.key}>
                        <TableCell>{row.key}</TableCell>
                        <TableCell className="text-right tabular-nums">
                          {number(row.views)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              ) : (
                <div
                  className="h-48 w-full min-w-0 sm:h-72"
                  aria-label="Page views by hour"
                >
                  <ResponsiveContainer width="100%" height="100%">
                    <AreaChart data={traffic.data?.rows ?? []}>
                      <CartesianGrid strokeDasharray="3 3" vertical={false} />
                      <XAxis
                        dataKey="key"
                        tickFormatter={(value) =>
                          new Date(value).toLocaleDateString(undefined, {
                            month: 'short',
                            day: 'numeric',
                            hour: 'numeric',
                          })
                        }
                        minTickGap={60}
                      />
                      <YAxis
                        width={42}
                        allowDecimals={false}
                        tickFormatter={(value) =>
                          new Intl.NumberFormat(undefined, {
                            notation: 'compact',
                          }).format(value)
                        }
                      />
                      <Tooltip
                        labelFormatter={(value) =>
                          new Date(String(value)).toLocaleString()
                        }
                      />
                      <Area
                        type="monotone"
                        dataKey="views"
                        name="Page views"
                        stroke="var(--chart-1)"
                        fill="var(--chart-1)"
                        fillOpacity={0.12}
                        isAnimationActive={false}
                      />
                    </AreaChart>
                  </ResponsiveContainer>
                </div>
              )}
            </TabsContent>
          </QueryContent>
        </DataSection>
      </Tabs>
      <section aria-labelledby="analytics-projects">
        <h2 id="analytics-projects" className="mb-3 text-base font-semibold">
          Top pages across projects
        </h2>
        <QueryContent
          title="Analytics"
          loading={query.isPending}
          error={query.error}
          empty={!query.data?.rows.length}
          retry={() => void query.refetch()}
        >
          <div className="md:hidden">
            <RankedList
              label="Top pages"
              items={(query.data?.rows ?? []).map((row) => ({
                id: `${row.project_id}:${row.key}`,
                title: row.key,
                subtitle: row.project_name,
                facts: [
                  { label: 'Views', value: number(row.views) },
                  { label: 'Visitors', value: number(row.visitors) },
                  { label: 'Sessions', value: number(row.sessions) },
                  {
                    label: 'Avg. time',
                    value:
                      row.avg_time_seconds == null
                        ? '—'
                        : `${number(row.avg_time_seconds)} s`,
                  },
                ],
              }))}
            />
          </div>
          <div className="hidden rounded-lg border md:block">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Page</TableHead>
                  <TableHead>Project</TableHead>
                  <TableHead className="text-right">Views</TableHead>
                  <TableHead className="text-right">Visitors</TableHead>
                  <TableHead className="text-right">Sessions</TableHead>
                  <TableHead className="text-right hidden md:table-cell">
                    Average time
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {query.data?.rows.map((row) => (
                  <TableRow key={`${row.project_id}:${row.key}`}>
                    <TableCell className="font-medium break-all">
                      {row.key}
                    </TableCell>
                    <TableCell>{row.project_name}</TableCell>
                    <TableCell className="text-right tabular-nums">
                      {number(row.views)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {number(row.visitors)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {number(row.sessions)}
                    </TableCell>
                    <TableCell className="text-right hidden md:table-cell tabular-nums">
                      {row.avg_time_seconds == null
                        ? '—'
                        : `${number(row.avg_time_seconds)} s`}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
          <div className="mt-4">
            <GlobalPagination view={view} total={query.data?.total ?? 0} />
          </div>
        </QueryContent>
      </section>
    </GlobalPage>
  )
}

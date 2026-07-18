import {
  getEventsCountOptions,
  getPropertyBreakdownOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, PropertyColumn } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { ArrowLeft, ChevronRight, Search } from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import {
  InsightsPanel,
  InsightsToggleButton,
  deriveBreakdownInsights,
  useInsightsOpen,
} from './insights'
import type { AiInsightContext, BreakdownFlavor } from './insights'

/**
 * Maximum rows fetched from the backend. The property-breakdown and
 * events-count endpoints both cap `limit` at 100, so this is the real ceiling
 * until pagination is added server-side.
 */
const MAX_ROWS = 100

export type DimensionKey =
  | 'events'
  | 'referrers'
  | 'browsers'
  | 'operating_systems'
  | 'devices'
  | 'countries'
  | 'regions'
  | 'cities'
  | 'channels'
  | 'languages'
  | 'pages'
  | 'utm_source'
  | 'utm_medium'
  | 'utm_campaign'
  | 'utm_term'
  | 'utm_content'

interface DimensionConfig {
  title: string
  singular: string
  plural: string
  /** When set, use the property-breakdown API with this group_by column. */
  groupBy?: PropertyColumn
  /** When set, use the dedicated events-count endpoint instead. */
  useEventsCount?: boolean
}

const DIMENSIONS: Record<DimensionKey, DimensionConfig> = {
  events: {
    title: 'Events',
    singular: 'event',
    plural: 'events',
    useEventsCount: true,
  },
  referrers: {
    title: 'Referrers',
    singular: 'referrer',
    plural: 'referrers',
    groupBy: 'referrer_hostname',
  },
  browsers: {
    title: 'Browsers',
    singular: 'browser',
    plural: 'browsers',
    groupBy: 'browser',
  },
  operating_systems: {
    title: 'Operating Systems',
    singular: 'operating system',
    plural: 'operating systems',
    groupBy: 'operating_system',
  },
  devices: {
    title: 'Devices',
    singular: 'device',
    plural: 'devices',
    groupBy: 'device_type',
  },
  countries: {
    title: 'Countries',
    singular: 'country',
    plural: 'countries',
    groupBy: 'country',
  },
  regions: {
    title: 'Regions',
    singular: 'region',
    plural: 'regions',
    groupBy: 'region',
  },
  cities: {
    title: 'Cities',
    singular: 'city',
    plural: 'cities',
    groupBy: 'city',
  },
  channels: {
    title: 'Traffic Channels',
    singular: 'channel',
    plural: 'channels',
    groupBy: 'channel',
  },
  languages: {
    title: 'Languages',
    singular: 'language',
    plural: 'languages',
    groupBy: 'language',
  },
  pages: {
    title: 'Pages',
    singular: 'page',
    plural: 'pages',
    groupBy: 'pathname',
  },
  utm_source: {
    title: 'UTM Sources',
    singular: 'source',
    plural: 'sources',
    groupBy: 'utm_source',
  },
  utm_medium: {
    title: 'UTM Mediums',
    singular: 'medium',
    plural: 'mediums',
    groupBy: 'utm_medium',
  },
  utm_campaign: {
    title: 'UTM Campaigns',
    singular: 'campaign',
    plural: 'campaigns',
    groupBy: 'utm_campaign',
  },
  utm_term: {
    title: 'UTM Terms',
    singular: 'term',
    plural: 'terms',
    groupBy: 'utm_term',
  },
  utm_content: {
    title: 'UTM Contents',
    singular: 'content',
    plural: 'contents',
    groupBy: 'utm_content',
  },
}

export function isDimensionKey(
  value: string | undefined
): value is DimensionKey {
  return !!value && value in DIMENSIONS
}

/** How each dimension's insights are narrated (see `deriveBreakdownInsights`). */
const FLAVORS: Record<DimensionKey, BreakdownFlavor> = {
  events: 'generic',
  referrers: 'acquisition',
  browsers: 'tech',
  operating_systems: 'tech',
  devices: 'tech',
  countries: 'geo',
  regions: 'geo',
  cities: 'geo',
  channels: 'acquisition',
  languages: 'geo',
  pages: 'content',
  utm_source: 'acquisition',
  utm_medium: 'acquisition',
  utm_campaign: 'acquisition',
  utm_term: 'acquisition',
  utm_content: 'acquisition',
}

interface DimensionListProps {
  project: ProjectResponse
  dimension: DimensionKey
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  onBack: () => void
}

interface Row {
  value: string
  count: number
  percentage: number
}

export function DimensionList({
  project,
  dimension,
  startDate,
  endDate,
  environment,
  onBack,
}: DimensionListProps) {
  const config = DIMENSIONS[dimension]
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const [search, setSearch] = React.useState('')
  const [insightsOpen, setInsightsOpen] = useInsightsOpen()

  /**
   * Only the `events` dimension currently has a per-value detail page
   * (`/analytics/events/:eventName`) that lists the visitors who triggered it.
   * Other dimensions don't have a segment-detail route yet, so their rows stay
   * non-clickable. The active date filter is propagated so the detail page
   * opens on the same range.
   */
  const getRowHref = (value: string): string | undefined => {
    if (dimension !== 'events') return undefined

    const params = new URLSearchParams()
    const filter = searchParams.get('filter')
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    if (filter) params.set('filter', filter)
    if (from) params.set('from', from)
    if (to) params.set('to', to)
    const qs = params.toString()

    return `/projects/${project.slug}/analytics/events/${encodeURIComponent(value)}${qs ? `?${qs}` : ''}`
  }

  const breakdownQuery = useQuery({
    ...getPropertyBreakdownOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: config.groupBy ?? 'event_name',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: MAX_ROWS,
      },
    }),
    enabled:
      !!startDate && !!endDate && !!config.groupBy && !config.useEventsCount,
  })

  const eventsQuery = useQuery({
    ...getEventsCountOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        limit: MAX_ROWS,
      },
    }),
    enabled: !!startDate && !!endDate && !!config.useEventsCount,
  })

  const isLoading = config.useEventsCount
    ? eventsQuery.isLoading
    : breakdownQuery.isLoading
  const error = config.useEventsCount ? eventsQuery.error : breakdownQuery.error

  const rows: Row[] = React.useMemo(() => {
    if (config.useEventsCount) {
      const data = eventsQuery.data
      if (!data) return []
      return data
        .slice()
        .sort((a, b) => b.count - a.count)
        .map((item) => ({
          value: item.event_name,
          count: item.count,
          percentage: item.percentage,
        }))
    }

    const data = breakdownQuery.data
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .slice()
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        value: item.value || (dimension === 'referrers' ? 'Direct' : 'Unknown'),
        count: item.count,
        percentage: total > 0 ? (item.count / total) * 100 : 0,
      }))
  }, [breakdownQuery.data, eventsQuery.data, config.useEventsCount, dimension])

  const filteredRows = React.useMemo(() => {
    const q = search.trim().toLowerCase()
    if (!q) return rows
    return rows.filter((row) => row.value.toLowerCase().includes(q))
  }, [rows, search])

  const reachedCap = rows.length >= MAX_ROWS

  const insights = React.useMemo(
    () =>
      deriveBreakdownInsights({
        rows,
        singular: config.singular,
        plural: config.plural,
        flavor: FLAVORS[dimension],
      }),
    [rows, config.singular, config.plural, dimension]
  )

  const aiContext = React.useMemo<AiInsightContext | undefined>(() => {
    if (rows.length === 0) return undefined
    return {
      surface: config.title.toLowerCase(),
      rangeStart: startDate?.toISOString(),
      rangeEnd: endDate?.toISOString(),
      stats: {
        unit: 'unique visitors',
        [config.plural.replace(/\s+/g, '_')]: rows
          .slice(0, 10)
          .map((row) => ({ name: row.value, visitors: row.count })),
      },
    }
  }, [rows, config.title, config.plural, startDate, endDate])

  return (
    <div className="flex flex-col gap-4 sm:gap-6">
      {insightsOpen && (
        <InsightsPanel
          insights={insights}
          isLoading={isLoading}
          aiContext={aiContext}
          project={{ id: project.id, slug: project.slug, name: project.name }}
        />
      )}
      <Card>
        <CardHeader>
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-center gap-2">
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7"
                onClick={onBack}
                aria-label="Back to overview"
              >
                <ArrowLeft className="h-4 w-4" />
              </Button>
              <div>
                <CardTitle>{config.title}</CardTitle>
                <CardDescription>
                  {startDate && endDate
                    ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                    : 'Select a date range'}
                </CardDescription>
              </div>
            </div>
            <div className="flex w-full items-center gap-2 sm:w-auto">
              <InsightsToggleButton
                open={insightsOpen}
                onToggle={setInsightsOpen}
              />
              <div className="relative w-full sm:w-[260px]">
                <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  placeholder={`Filter ${config.plural}...`}
                  className="pl-8"
                />
              </div>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="space-y-2 py-4">
              {[...Array(10)].map((_, i) => (
                <div
                  key={`skel-${i}`}
                  className="flex items-center justify-between"
                >
                  <div className="h-4 w-[200px] bg-muted animate-pulse rounded" />
                  <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
                </div>
              ))}
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-8 text-center">
              <p className="text-sm text-muted-foreground mb-2">
                Failed to load {config.plural}
              </p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => window.location.reload()}
              >
                Try again
              </Button>
            </div>
          ) : !filteredRows.length ? (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <p className="text-sm text-muted-foreground">
                {rows.length === 0
                  ? 'No data available for the selected period'
                  : `No ${config.plural} match "${search}"`}
              </p>
            </div>
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[60px] text-right">#</TableHead>
                    <TableHead>{capitalize(config.singular)}</TableHead>
                    <TableHead className="text-right">Visitors</TableHead>
                    <TableHead className="text-right">Share</TableHead>
                    <TableHead className="hidden md:table-cell w-[200px]" />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {filteredRows.map((row, idx) => {
                    const rank =
                      rows.findIndex((r) => r.value === row.value) + 1
                    const href = getRowHref(row.value)
                    const clickable = !!href
                    return (
                      <TableRow
                        key={`${row.value}-${idx}`}
                        className={
                          clickable ? 'cursor-pointer hover:bg-muted/50' : ''
                        }
                        onClick={(e) => {
                          if (!href) return
                          if (e.metaKey || e.ctrlKey) {
                            window.open(href, '_blank')
                          } else {
                            navigate(href)
                          }
                        }}
                      >
                        <TableCell className="text-right text-muted-foreground tabular-nums">
                          {rank}
                        </TableCell>
                        <TableCell className="font-medium break-all">
                          <span className="inline-flex items-center gap-1">
                            {row.value}
                            {clickable && (
                              <ChevronRight className="h-3 w-3 text-muted-foreground" />
                            )}
                          </span>
                        </TableCell>
                        <TableCell className="text-right font-mono tabular-nums">
                          {row.count.toLocaleString()}
                        </TableCell>
                        <TableCell className="text-right tabular-nums text-muted-foreground">
                          {row.percentage.toFixed(1)}%
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                            <div
                              className="absolute inset-y-0 left-0 bg-primary rounded-full"
                              style={{
                                width: `${Math.min(row.percentage, 100)}%`,
                              }}
                            />
                          </div>
                        </TableCell>
                      </TableRow>
                    )
                  })}
                </TableBody>
              </Table>
            </div>
          )}
        </CardContent>
        {!isLoading && !error && rows.length > 0 && (
          <CardFooter className="flex-col items-start gap-1 text-sm">
            <div className="leading-none text-muted-foreground">
              Showing {filteredRows.length.toLocaleString()} of{' '}
              {rows.length.toLocaleString()} {config.plural} by unique visitors
            </div>
            {reachedCap && (
              <div className="leading-none text-xs text-muted-foreground">
                Limited to top {MAX_ROWS} by the analytics API. Narrow the date
                range to surface less-frequent {config.plural}.
              </div>
            )}
          </CardFooter>
        )}
      </Card>
    </div>
  )
}

function capitalize(s: string): string {
  return s.length === 0 ? s : s.charAt(0).toUpperCase() + s.slice(1)
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironmentsOptions,
  getVisitorFacetsOptions,
  getVisitorsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ProjectResponse,
  VisitorInfo,
  VisitorFacets,
  VisitorSegmentFilters,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import { TimeRangeFilter } from '@/components/ui/time-range-filter'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { TableColumnPicker } from '@/components/table/TableColumnPicker'
import { useTableColumns } from '@/components/table/useTableColumns'
import {
  captureListReturnState,
  useListScrollRestoration,
  type ListReturnState,
} from '@/components/table/list-navigation'
import {
  Callout,
  DataTable,
  PageHeader,
  PageState,
  RecordLink,
  fmtDateTime,
  fmtNumber,
  fmtRelativeTime,
  useUrlState,
  type DataTableColumn,
} from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  Bot,
  RefreshCw,
  SlidersHorizontal,
  User,
  Users,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { Link, useLocation } from 'react-router'
import { resolveTimeRange } from '@/lib/time-range-filter'
import { FacetCombobox } from './FacetCombobox'
import { crawlerLabel } from '@/components/analytics/traffic-filters-shared'
import {
  VISITOR_COLUMNS,
  DEFAULT_VISITOR_COLUMNS,
  VISITOR_COLUMNS_STORAGE_KEY,
  type VisitorColumn,
} from './visitor-columns'

interface VisitorsListProps {
  project: ProjectResponse
  listState?: ListReturnState | null
  onRememberList?: (state: ListReturnState) => void
}

const FILTERS = [
  { key: 'filter_country', label: 'Country', facet: 'country' },
  { key: 'filter_region', label: 'Region', facet: 'region' },
  { key: 'filter_city', label: 'City', facet: 'city' },
  { key: 'filter_channel', label: 'Channel', facet: 'channel' },
  { key: 'filter_referrer', label: 'Referrer', facet: 'referrer' },
] as const satisfies readonly {
  key: keyof VisitorSegmentFilters
  label: string
  facet: keyof VisitorFacets
}[]

type FilterKey = (typeof FILTERS)[number]['key']

function positiveInteger(value: string | undefined, fallback: number) {
  const parsed = Number(value)
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback
}

function browserName(agent: string) {
  if (/Edg\/|Edge\//.test(agent)) return 'Edge'
  if (/OPR\/|Opera/.test(agent)) return 'Opera'
  if (/Firefox\/|FxiOS\//.test(agent)) return 'Firefox'
  if (/Chrome\/|CriOS\//.test(agent)) return 'Chrome'
  if (/Safari\//.test(agent)) return 'Safari'
  return 'Unknown'
}

function osName(agent: string) {
  if (/iPhone|iPad|iPod/.test(agent)) return 'iOS'
  if (agent.includes('Android')) return 'Android'
  if (agent.includes('CrOS')) return 'ChromeOS'
  if (agent.includes('Windows')) return 'Windows'
  if (agent.includes('Mac OS')) return 'macOS'
  if (agent.includes('Linux')) return 'Linux'
  return 'Unknown'
}

export function VisitorsList({
  project,
  listState,
  onRememberList,
}: VisitorsListProps) {
  const { state, patch } = useUrlState()
  const location = useLocation()
  const url = location.pathname + location.search
  const containerRef = useRef<HTMLDivElement>(null)
  const [saved] = useState(() => (listState?.url === url ? listState : null))
  const [initialNow] = useState(Date.now)
  const [refreshedAt, setRefreshedAt] = useState<number | null>(null)
  const page = positiveInteger(state.page, 1)
  const requestedLimit = positiveInteger(state.limit, 25)
  const limit = [10, 25, 50, 100].includes(requestedLimit) ? requestedLimit : 25
  const timeRange = state.time_range || '30d'
  const visitorType = state.visitor_type === 'humans' ? 'humans' : 'all'
  const activityOnly = state.activity !== 'all'
  const environment = positiveInteger(state.environment, 0) || undefined
  const filters = Object.fromEntries(
    FILTERS.filter(({ key }) => state[key]?.trim()).map(({ key }) => [
      key,
      state[key]!.trim(),
    ])
  ) as Partial<Record<FilterKey, string>>
  const activeFilters = FILTERS.filter(({ key }) => !!filters[key])
  const filterCount =
    activeFilters.length +
    Number(visitorType !== 'all') +
    Number(!activityOnly) +
    Number(!!environment)
  const filtersExpanded = state.filters === 'open'
  const hasFilters = filterCount > 0 || timeRange !== '30d'
  const { visibleKeys, changeColumns, resetColumns } = useTableColumns({
    options: VISITOR_COLUMNS,
    defaults: DEFAULT_VISITOR_COLUMNS,
    storageKey: VISITOR_COLUMNS_STORAGE_KEY,
  })
  const dateRange = useMemo(() => {
    if (refreshedAt === null && saved?.timeRange === timeRange)
      return saved.dateRange
    const resolved = resolveTimeRange(timeRange, refreshedAt ?? initialNow)
    return { from: resolved.from, to: resolved.to }
  }, [saved, timeRange, initialNow, refreshedAt])

  const queryFilters = {
    project_id: project.id,
    start_date: dateRange.from,
    end_date: dateRange.to,
    environment_id: environment,
    include_crawlers: visitorType === 'humans' ? false : undefined,
    has_activity_only: activityOnly,
    ...filters,
  }
  const facets = useQuery({
    ...getVisitorFacetsOptions({
      query: { ...queryFilters, per_facet_limit: 50 },
    }),
    enabled: filtersExpanded,
  })
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: filtersExpanded || visibleKeys.includes('environment'),
  })
  const query = useQuery(
    getVisitorsOptions({
      query: { ...queryFilters, offset: (page - 1) * limit, limit },
    })
  )
  const { data, isLoading, error } = query
  const totalPages = Math.max(1, Math.ceil((data?.filtered_count ?? 0) / limit))
  useEffect(() => {
    if (query.isSuccess && page > totalPages)
      patch({ page: totalPages > 1 ? totalPages : undefined })
  }, [query.isSuccess, page, totalPages, patch])
  useListScrollRestoration(containerRef, saved, !!data, url)

  const updateFilter = (key: FilterKey, value: string | undefined) =>
    patch({ [key]: value?.trim(), page: undefined })
  const clearFilters = () =>
    patch({
      ...Object.fromEntries(FILTERS.map(({ key }) => [key, undefined])),
      environment: undefined,
      visitor_type: undefined,
      activity: undefined,
      time_range: undefined,
      page: undefined,
    })
  const rememberList = () =>
    onRememberList?.(
      captureListReturnState(containerRef.current, {
        url,
        timeRange,
        dateRange,
      })
    )
  const renderLocationFilter = (key: FilterKey, value: string) => (
    <Button
      type="button"
      variant="link"
      size="sm"
      className="h-auto max-w-48 truncate p-0 font-normal text-foreground decoration-muted-foreground underline-offset-4"
      aria-label={`Filter by ${value}`}
      aria-pressed={filters[key] === value}
      onClick={() =>
        updateFilter(key, filters[key] === value ? undefined : value)
      }
    >
      {value}
    </Button>
  )

  const renderValue = (key: VisitorColumn, visitor: VisitorInfo) => {
    switch (key) {
      case 'visitor': {
        const Icon = visitor.is_crawler ? Bot : User
        const label = visitor.is_crawler
          ? crawlerLabel(visitor.crawler_name, 'Crawler')
          : 'Human visitor'
        return (
          <RecordLink
            to={`/projects/${project.slug}/analytics/visitors/${visitor.id}`}
            state={{ listReturnTo: url }}
            onClick={rememberList}
            aria-label={`View ${label} details`}
          >
            <span className="inline-flex items-center gap-2">
              <Icon
                className="size-4 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              <span className="block max-w-48 truncate" title={label}>
                {label}
              </span>
            </span>
          </RecordLink>
        )
      }
      case 'location': {
        const parts = [
          ['filter_city', visitor.city],
          ['filter_region', visitor.region],
          ['filter_country', visitor.country],
        ] as const
        return (
          <div className="max-w-64 space-y-1">
            {parts
              .filter(
                ([, value], index) =>
                  value && (index !== 1 || value !== visitor.city)
              )
              .map(([filter, value]) => (
                <div key={filter}>{renderLocationFilter(filter, value!)}</div>
              ))}
            {!visitor.city && !visitor.region && !visitor.country && (
              <span className="text-muted-foreground">Unknown</span>
            )}
          </div>
        )
      }
      case 'source':
        return (
          <div className="max-w-48 space-y-1">
            {visitor.first_channel && (
              <div>
                {renderLocationFilter('filter_channel', visitor.first_channel)}
              </div>
            )}
            {visitor.first_referrer_hostname && (
              <div>
                {renderLocationFilter(
                  'filter_referrer',
                  visitor.first_referrer_hostname
                )}
              </div>
            )}
            {!visitor.first_channel && !visitor.first_referrer_hostname && (
              <span className="text-muted-foreground">Direct</span>
            )}
          </div>
        )
      case 'browser':
        return (
          <div>
            <p>{browserName(visitor.user_agent ?? '')}</p>
            <p className="text-xs text-muted-foreground">
              {osName(visitor.user_agent ?? '')}
            </p>
          </div>
        )
      case 'first_seen':
      case 'last_seen': {
        const value = visitor[key]
        return (
          <time
            dateTime={value}
            title={value}
            className="block whitespace-nowrap"
          >
            <span className="block">{fmtRelativeTime(value)}</span>
            <span className="text-xs text-muted-foreground">
              {fmtDateTime(value)}
            </span>
          </time>
        )
      }
      case 'environment':
        return (
          environments.data?.find((item) => item.id === visitor.environment_id)
            ?.name ?? `Environment ${visitor.environment_id}`
        )
      default:
        return (
          <span
            className={`block max-w-64 truncate ${key === 'current_page' || key === 'ip_address' ? 'font-mono' : ''}`}
            title={visitor[key] ?? undefined}
          >
            {visitor[key] || '—'}
          </span>
        )
    }
  }
  const columns: DataTableColumn<VisitorInfo>[] = VISITOR_COLUMNS.filter(
    ({ key }) => visibleKeys.includes(key)
  ).map(({ key, label }) => ({
    key,
    header: label,
    render: (visitor) => renderValue(key, visitor),
  }))

  return (
    <div ref={containerRef} className="w-full min-w-0 space-y-6">
      <PageHeader
        headingLevel="h2"
        title="Visitors"
        description={
          data
            ? `${fmtNumber(data.filtered_count)} ${activityOnly ? 'visitors with activity' : 'visitors'} · ${timeRange === '30d' ? 'Last 30 days' : 'Selected time range'}`
            : 'Explore visitors and their activity.'
        }
        actions={
          <>
            <Button variant="outline" size="sm" asChild>
              <Link to={`/projects/${project.slug}/analytics/activity`}>
                Explore activity with AI
              </Link>
            </Button>
            <Button
              variant="outline"
              size="sm"
              aria-disabled={query.isFetching}
              onClick={() => {
                if (query.isFetching) return
                if (timeRange.startsWith('custom:')) void query.refetch()
                else setRefreshedAt(Date.now())
              }}
            >
              <RefreshCw className="mr-2 size-4" aria-hidden="true" />
              {query.isFetching ? 'Refreshing…' : 'Refresh'}
            </Button>
          </>
        }
      />
      <div className="flex flex-wrap items-center gap-2">
        <fieldset aria-label="Time range" className="min-w-0">
          <TimeRangeFilter
            value={timeRange}
            customLabel={timeRange === '30d' ? '30d' : undefined}
            onChange={(value) => patch({ time_range: value, page: undefined })}
          />
        </fieldset>
        <Select
          value={visitorType}
          onValueChange={(value) =>
            patch({
              visitor_type: value === 'all' ? undefined : value,
              page: undefined,
            })
          }
        >
          <SelectTrigger aria-label="Visitor type" className="w-40">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All visitors</SelectItem>
            <SelectItem value="humans">Humans only</SelectItem>
          </SelectContent>
        </Select>
        <Button
          type="button"
          variant={filtersExpanded ? 'secondary' : 'outline'}
          size="sm"
          aria-expanded={filtersExpanded}
          aria-controls="visitors-filter-panel"
          onClick={() =>
            patch({ filters: filtersExpanded ? undefined : 'open' })
          }
        >
          <SlidersHorizontal className="mr-2 size-4" aria-hidden="true" />
          Filters
          {filterCount > 0 && (
            <Badge variant="secondary" className="ml-2 tabular-nums">
              {filterCount}
            </Badge>
          )}
        </Button>
        <div className="ml-auto">
          <TableColumnPicker
            options={VISITOR_COLUMNS}
            visibleKeys={visibleKeys}
            onChange={changeColumns}
            onReset={resetColumns}
          />
        </div>
      </div>
      {filtersExpanded && (
        <div
          id="visitors-filter-panel"
          className="space-y-4 rounded-lg border bg-muted/30 p-4"
        >
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 xl:grid-cols-3">
            {FILTERS.map(({ key, label, facet }) => (
              <FacetCombobox
                key={key}
                label={label}
                value={filters[key]}
                options={facets.data?.[facet] ?? []}
                onChange={(value) => updateFilter(key, value)}
                loading={facets.isLoading}
                error={!!facets.error}
                onRetry={() => void facets.refetch()}
              />
            ))}
            <Select
              value={environment ? String(environment) : 'all'}
              onValueChange={(value) =>
                patch({
                  environment: value === 'all' ? undefined : value,
                  page: undefined,
                })
              }
            >
              <SelectTrigger aria-label="Environment">
                <SelectValue placeholder="All environments" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All environments</SelectItem>
                {environments.data?.map((item) => (
                  <SelectItem key={item.id} value={String(item.id)}>
                    {item.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {environments.error && (
            <Callout tone="error" title="Could not load environments">
              <Button
                variant="outline"
                size="sm"
                onClick={() => void environments.refetch()}
              >
                Retry environments
              </Button>
            </Callout>
          )}
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="space-y-1">
              <div className="flex items-center gap-2">
                <Switch
                  id="visitors-activity-only"
                  checked={activityOnly}
                  onCheckedChange={(checked) =>
                    patch({
                      activity: checked ? undefined : 'all',
                      page: undefined,
                    })
                  }
                  aria-describedby="visitors-activity-help"
                />
                <Label htmlFor="visitors-activity-only">
                  Only visitors with activity
                </Label>
              </div>
              <p
                id="visitors-activity-help"
                className="text-xs text-muted-foreground"
              >
                Exclude visitor records with no recorded events or sessions.
              </p>
            </div>
            {hasFilters && (
              <Button variant="ghost" size="sm" onClick={clearFilters}>
                Clear filters
              </Button>
            )}
          </div>
        </div>
      )}
      {(activeFilters.length > 0 || environment || !activityOnly) && (
        <div
          className="flex flex-wrap items-center gap-2"
          aria-label="Active visitor filters"
        >
          {activeFilters.map(({ key, label }) => (
            <Button
              key={key}
              variant="outline"
              size="sm"
              onClick={() => updateFilter(key, undefined)}
              aria-label={`Remove ${label} filter`}
            >
              <span className="max-w-48 truncate">
                {label}: {filters[key]}
              </span>
              <X className="ml-1 size-3" aria-hidden="true" />
            </Button>
          ))}
          {environment && (
            <Badge variant="secondary">
              Environment:{' '}
              {environments.data?.find((item) => item.id === environment)
                ?.name ?? environment}
            </Badge>
          )}
          {!activityOnly && (
            <Badge variant="secondary">
              Including visitors without activity
            </Badge>
          )}
          <Button variant="ghost" size="sm" onClick={clearFilters}>
            Clear filters
          </Button>
        </div>
      )}
      {error && data && (
        <Callout tone="error" title="Visitors could not refresh">
          <div className="flex flex-wrap items-center gap-3">
            <span>Showing previously loaded visitors.</span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void query.refetch()}
            >
              Retry
            </Button>
          </div>
        </Callout>
      )}
      {error && !data ? (
        <div className="rounded-lg border bg-card text-card-foreground">
          <PageState
            size="compact"
            variant="failed"
            icon={AlertCircle}
            title="Could not load visitors"
            description="Try again to load visitors for this project."
            action={<Button onClick={() => void query.refetch()}>Retry</Button>}
          />
        </div>
      ) : !isLoading && data?.visitors.length === 0 ? (
        <div className="rounded-lg border bg-card text-card-foreground">
          <PageState
            size="compact"
            variant="empty"
            icon={Users}
            title={hasFilters ? 'No matching visitors' : 'No visitors yet'}
            description={
              hasFilters
                ? 'Try another time range or clear the filters.'
                : 'Visitors with recorded activity will appear here.'
            }
            action={
              hasFilters ? (
                <Button variant="outline" onClick={clearFilters}>
                  Clear filters
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : (
        <DataTable
          aria-label="Visitors"
          columns={columns}
          rows={data?.visitors ?? []}
          rowKey={(visitor) => visitor.id}
          isLoading={isLoading}
        />
      )}
      {data && (
        <ResponsivePagination
          page={page}
          pageSize={limit}
          total={data.filtered_count}
          totalPages={totalPages}
          onPageChange={(value) =>
            patch({ page: value > 1 ? value : undefined })
          }
          onPageSizeChange={(value) =>
            patch({ limit: value === 25 ? undefined : value, page: undefined })
          }
          pageSizeOptions={[10, 25, 50, 100]}
          ariaLabel="Visitor pagination"
          pageSizeAriaLabel="Visitors per page"
        />
      )}
    </div>
  )
}

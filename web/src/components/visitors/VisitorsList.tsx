import {
  getVisitorFacetsOptions,
  getVisitorsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse, VisitorInfo } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Label } from '@/components/ui/label'
import { useQuery } from '@tanstack/react-query'
import { format, formatDistanceToNow } from 'date-fns'
import {
  Globe,
  Bot,
  User,
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  ExternalLink,
  SlidersHorizontal,
  X,
} from 'lucide-react'
import * as React from 'react'
import { useNavigate } from 'react-router'
import { Skeleton } from '@/components/ui/skeleton'
import { FacetCombobox, type FacetOption } from './FacetCombobox'

interface VisitorsListProps {
  project: ProjectResponse
}

function countryCodeToFlag(countryCode: string | null | undefined): string {
  if (!countryCode || countryCode.length !== 2) return ''
  const codePoints = countryCode
    .toUpperCase()
    .split('')
    .map((char) => 127397 + char.charCodeAt(0))
  return String.fromCodePoint(...codePoints)
}

function getBrowserInfo(userAgent: string): { name: string; icon: string } {
  if (userAgent.includes('Edge') || userAgent.includes('Edg/')) {
    return { name: 'Edge', icon: 'edge' }
  } else if (userAgent.includes('Chrome') && !userAgent.includes('Chromium')) {
    return { name: 'Chrome', icon: 'chrome' }
  } else if (userAgent.includes('Safari') && !userAgent.includes('Chrome')) {
    return { name: 'Safari', icon: 'safari' }
  } else if (userAgent.includes('Firefox')) {
    return { name: 'Firefox', icon: 'firefox' }
  } else if (userAgent.includes('Opera') || userAgent.includes('OPR')) {
    return { name: 'Opera', icon: 'opera' }
  } else if (userAgent.includes('bot') || userAgent.includes('Bot')) {
    return { name: 'Bot', icon: 'bot' }
  }
  return { name: 'Unknown', icon: 'unknown' }
}

function getOSName(userAgent: string): string {
  if (userAgent.includes('Windows')) return 'Windows'
  if (userAgent.includes('Mac OS')) return 'macOS'
  if (userAgent.includes('Linux') && !userAgent.includes('Android')) return 'Linux'
  if (userAgent.includes('Android')) return 'Android'
  if (userAgent.includes('iPhone') || userAgent.includes('iPad')) return 'iOS'
  if (userAgent.includes('CrOS')) return 'ChromeOS'
  return 'Unknown'
}

type SegmentFilters = {
  filter_country?: string
  filter_region?: string
  filter_city?: string
  filter_channel?: string
  filter_referrer?: string
}

type SegmentFilterKey = keyof SegmentFilters

const FILTER_LABELS: Record<SegmentFilterKey, string> = {
  filter_country: 'Country',
  filter_region: 'Region',
  filter_city: 'City',
  filter_channel: 'Channel',
  filter_referrer: 'Referrer',
}

/** Filter groups rendered inline. Only visitor-row dimensions are supported
 *  — see analytics service: event-row filters were dropped to keep the page
 *  fast at high event volume. */
const FILTER_GROUPS: {
  title: string
  keys: SegmentFilterKey[]
}[] = [
  {
    title: 'Location',
    keys: ['filter_country', 'filter_region', 'filter_city'],
  },
  {
    title: 'Acquisition',
    keys: ['filter_channel', 'filter_referrer'],
  },
]

/** Filters that should render a country flag next to each option. */
const FLAG_FILTERS = new Set<SegmentFilterKey>(['filter_country'])

/** Map each filter key to the facet response field on the API. */
const FILTER_TO_FACET: Record<SegmentFilterKey, string> = {
  filter_country: 'country',
  filter_region: 'region',
  filter_city: 'city',
  filter_channel: 'channel',
  filter_referrer: 'referrer',
}

function activeFilterEntries(filters: SegmentFilters) {
  return (Object.entries(filters) as [SegmentFilterKey, string | undefined][])
    .filter(([, value]) => value && value.trim() !== '')
    .map(([key, value]) => [key, value as string] as const)
}

export function VisitorsList({ project }: VisitorsListProps) {
  const navigate = useNavigate()
  const [page, setPage] = React.useState(1)
  const [limit, setLimit] = React.useState(25)
  const [crawlerFilter, setCrawlerFilter] = React.useState<
    'all' | 'humans' | 'crawlers'
  >('all')
  const [hideGhostVisitors, setHideGhostVisitors] = React.useState(true)
  const [filters, setFilters] = React.useState<SegmentFilters>({})
  const [filtersExpanded, setFiltersExpanded] = React.useState(false)

  const updateFilter = React.useCallback(
    (key: SegmentFilterKey, value: string | undefined) => {
      setPage(1)
      setFilters((prev) => {
        const next = { ...prev }
        if (!value || value.trim() === '') {
          delete next[key]
        } else {
          next[key] = value.trim()
        }
        return next
      })
    },
    []
  )

  const clearFilters = React.useCallback(() => {
    setPage(1)
    setFilters({})
  }, [])

  const activeFilters = React.useMemo(
    () => activeFilterEntries(filters),
    [filters]
  )

  // Default date range: last 30 days
  const endDate = React.useMemo(() => {
    const date = new Date()
    date.setHours(23, 59, 59, 999)
    return date
  }, [])

  const startDate = React.useMemo(() => {
    const date = new Date()
    date.setDate(date.getDate() - 30)
    date.setHours(0, 0, 0, 0)
    return date
  }, [])

  // Populate filter dropdowns with real values + visitor counts. The query is
  // re-issued when any filter changes so each dropdown reflects the *other*
  // filters' narrowed pool — the backend handles excluding the dimension
  // being aggregated from its own filter.
  const { data: facetsData, isLoading: facetsLoading } = useQuery({
    ...getVisitorFacetsOptions({
      query: {
        project_id: project.id,
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        include_crawlers:
          crawlerFilter === 'all'
            ? undefined
            : crawlerFilter === 'crawlers'
              ? true
              : false,
        has_activity_only: hideGhostVisitors ? undefined : false,
        per_facet_limit: 50,
        ...filters,
      },
    }),
    // Only fetch facets once the user has expanded the filter section.
    enabled: filtersExpanded || activeFilters.length > 0,
  })

  const getFacetOptions = React.useCallback(
    (key: SegmentFilterKey): FacetOption[] => {
      if (!facetsData) return []
      const field = FILTER_TO_FACET[key]
      const list = (facetsData as Record<string, unknown>)[field]
      if (!Array.isArray(list)) return []
      return list.map((row) => ({
        value: String((row as { value: unknown }).value ?? ''),
        count: Number((row as { count: unknown }).count ?? 0),
        code: (row as { code?: string | null }).code ?? null,
      }))
    },
    [facetsData]
  )

  const { data, isLoading, error, refetch } = useQuery({
    ...getVisitorsOptions({
      query: {
        project_id: project.id,
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        offset: (page - 1) * limit,
        limit,
        include_crawlers:
          crawlerFilter === 'all'
            ? undefined
            : crawlerFilter === 'crawlers'
              ? true
              : false,
        has_activity_only: hideGhostVisitors ? undefined : false,
        ...filters,
      },
    }),
  })

  const totalPages = React.useMemo(() => {
    if (!data) return 0
    return Math.ceil(data.filtered_count / limit)
  }, [data, limit])

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div>
              <CardTitle>Visitors</CardTitle>
              <CardDescription>
                {data
                  ? `${data.filtered_count.toLocaleString()} visitors found`
                  : 'Browse and analyze visitor sessions'}
              </CardDescription>
            </div>
            <div className="flex flex-wrap items-center gap-2 sm:gap-4">
              <div className="flex items-center gap-2">
                <Switch
                  id="hide-ghost"
                  checked={hideGhostVisitors}
                  onCheckedChange={setHideGhostVisitors}
                />
                <Label
                  htmlFor="hide-ghost"
                  className="text-sm cursor-pointer whitespace-nowrap"
                >
                  Hide ghost visitors
                </Label>
              </div>
              <Button
                variant={filtersExpanded ? 'secondary' : 'outline'}
                size="sm"
                onClick={() => setFiltersExpanded((v) => !v)}
                className="gap-1.5 whitespace-nowrap"
                aria-expanded={filtersExpanded}
                aria-controls="visitors-filter-panel"
              >
                <SlidersHorizontal className="h-3.5 w-3.5" />
                Filters
                {activeFilters.length > 0 && (
                  <Badge
                    variant="secondary"
                    className="ml-0.5 h-4 min-w-[16px] px-1 text-[10px]"
                  >
                    {activeFilters.length}
                  </Badge>
                )}
                <ChevronDown
                  className={`h-3.5 w-3.5 transition-transform ${
                    filtersExpanded ? 'rotate-180' : ''
                  }`}
                />
              </Button>
              <Select
                value={crawlerFilter}
                onValueChange={(value: 'all' | 'humans' | 'crawlers') =>
                  setCrawlerFilter(value)
                }
              >
                <SelectTrigger className="w-[120px] sm:w-[140px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Visitors</SelectItem>
                  <SelectItem value="humans">Humans Only</SelectItem>
                  <SelectItem value="crawlers">Crawlers Only</SelectItem>
                </SelectContent>
              </Select>
              <Select
                value={limit.toString()}
                onValueChange={(value) => setLimit(parseInt(value))}
              >
                <SelectTrigger className="w-[90px] sm:w-[100px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="10">10 / page</SelectItem>
                  <SelectItem value="25">25 / page</SelectItem>
                  <SelectItem value="50">50 / page</SelectItem>
                  <SelectItem value="100">100 / page</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {filtersExpanded && (
            <div
              id="visitors-filter-panel"
              className="mb-5 rounded-lg border bg-muted/20 p-3 sm:p-4"
            >
              <div className="flex items-center justify-between mb-3">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium">Filters</span>
                  {activeFilters.length > 0 && (
                    <Badge
                      variant="secondary"
                      className="h-5 px-1.5 text-[10px]"
                    >
                      {activeFilters.length} active
                    </Badge>
                  )}
                </div>
                {activeFilters.length > 0 && (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 px-2 text-xs gap-1"
                    onClick={clearFilters}
                  >
                    <X className="h-3 w-3" /> Clear all
                  </Button>
                )}
              </div>

              {FILTER_GROUPS.map((group) => (
                <div key={group.title} className="mb-4 last:mb-0">
                  <div className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground mb-1.5">
                    {group.title}
                  </div>
                  <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-2">
                    {group.keys.map((key) => (
                      <FacetCombobox
                        key={key}
                        label={FILTER_LABELS[key]}
                        value={filters[key]}
                        options={getFacetOptions(key)}
                        onChange={(next) => updateFilter(key, next)}
                        withFlag={FLAG_FILTERS.has(key)}
                        loading={facetsLoading}
                      />
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
          {isLoading ? (
            <div className="space-y-2">
              {[...Array(8)].map((_, i) => (
                <Skeleton key={i} className="h-12 w-full rounded" />
              ))}
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center py-12">
              <p className="text-muted-foreground mb-2">
                Failed to load visitors
              </p>
              <Button variant="outline" onClick={() => refetch()}>
                Try again
              </Button>
            </div>
          ) : !data || data.visitors.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12">
              <User className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-muted-foreground">No visitors found</p>
            </div>
          ) : (
            <>
              <TooltipProvider>
                <div className="overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead className="w-[200px] sm:w-[280px]">Visitor</TableHead>
                      <TableHead>Location</TableHead>
                      <TableHead className="hidden md:table-cell">Source</TableHead>
                      <TableHead className="hidden lg:table-cell">Browser / OS</TableHead>
                      <TableHead>First Seen</TableHead>
                      <TableHead>Last Seen</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {data.visitors.map((visitor: VisitorInfo) => (
                      <VisitorRow
                        key={visitor.visitor_id}
                        visitor={visitor}
                        onClick={() =>
                          navigate(
                            `/projects/${project.slug}/analytics/visitors/${visitor.id}`
                          )
                        }
                        onFilter={updateFilter}
                        activeFilters={filters}
                      />
                    ))}
                  </TableBody>
                </Table>
                </div>
              </TooltipProvider>

              {/* Pagination */}
              {totalPages > 1 && (
                <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between mt-6">
                  <div className="text-sm text-muted-foreground">
                    <span className="hidden sm:inline">
                      Showing {(page - 1) * limit + 1} to{' '}
                      {Math.min(page * limit, data.filtered_count)} of{' '}
                      {data.filtered_count} visitors
                    </span>
                    <span className="sm:hidden">
                      {page} / {totalPages}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setPage((p) => Math.max(1, p - 1))}
                      disabled={page === 1}
                    >
                      <ChevronLeft className="h-4 w-4" />
                      <span className="hidden sm:inline">Previous</span>
                    </Button>
                    <div className="hidden sm:flex items-center gap-1">
                      {[...Array(Math.min(5, totalPages))].map((_, idx) => {
                        const pageNum = page - 2 + idx
                        if (pageNum < 1 || pageNum > totalPages) return null
                        return (
                          <Button
                            key={pageNum}
                            variant={pageNum === page ? 'default' : 'outline'}
                            size="sm"
                            onClick={() => setPage(pageNum)}
                            className="w-10"
                          >
                            {pageNum}
                          </Button>
                        )
                      })}
                    </div>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setPage((p) => Math.min(totalPages, p + 1))
                      }
                      disabled={page === totalPages}
                    >
                      <span className="hidden sm:inline">Next</span>
                      <ChevronRight className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              )}
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

interface VisitorRowProps {
  visitor: VisitorInfo
  onClick: () => void
  onFilter: (key: SegmentFilterKey, value: string | undefined) => void
  activeFilters: SegmentFilters
}

function VisitorRow({
  visitor,
  onClick,
  onFilter,
  activeFilters,
}: VisitorRowProps) {
  const browserInfo = visitor.user_agent
    ? getBrowserInfo(visitor.user_agent)
    : null
  const osName = visitor.user_agent ? getOSName(visitor.user_agent) : null
  const flag = countryCodeToFlag(visitor.country_code)
  const lastSeenDate = new Date(visitor.last_seen)
  const firstSeenDate = new Date(visitor.first_seen)

  const handleFilter = (
    e: React.MouseEvent,
    key: SegmentFilterKey,
    value: string | null | undefined
  ) => {
    if (!value) return
    e.stopPropagation()
    onFilter(key, activeFilters[key] === value ? undefined : value)
  }

  return (
    <TableRow
      className="cursor-pointer"
      onClick={onClick}
    >
      {/* Visitor identity */}
      <TableCell>
        <div className="flex items-center gap-3">
          <div
            className={`flex h-8 w-8 items-center justify-center rounded-full flex-shrink-0 ${
              visitor.is_crawler
                ? 'bg-amber-100 text-amber-600 dark:bg-amber-900/30 dark:text-amber-400'
                : 'bg-blue-100 text-blue-600 dark:bg-blue-900/30 dark:text-blue-400'
            }`}
          >
            {visitor.is_crawler ? (
              <Bot className="h-4 w-4" />
            ) : (
              <User className="h-4 w-4" />
            )}
          </div>
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="font-mono text-sm truncate">
                {visitor.visitor_id?.substring(0, 12)}
              </span>
              <Badge
                variant={visitor.is_crawler ? 'warning' : 'secondary'}
                className="text-[10px] px-1.5 py-0"
              >
                {visitor.is_crawler
                  ? visitor.crawler_name || 'Bot'
                  : 'Human'}
              </Badge>
            </div>
          </div>
        </div>
      </TableCell>

      {/* Location */}
      <TableCell>
        <div className="flex items-center gap-2 min-w-0">
          {visitor.country ? (
            <button
              type="button"
              onClick={(e) => handleFilter(e, 'filter_country', visitor.country)}
              title={`Filter by ${visitor.country}`}
              className="text-base leading-none hover:scale-110 transition-transform"
            >
              {flag || <Globe className="h-4 w-4 text-muted-foreground" />}
            </button>
          ) : (
            <Globe className="h-4 w-4 text-muted-foreground flex-shrink-0" />
          )}
          <div className="flex items-center gap-1 text-sm truncate max-w-[220px]">
            {visitor.city && (
              <>
                <button
                  type="button"
                  onClick={(e) => handleFilter(e, 'filter_city', visitor.city)}
                  className="hover:underline truncate"
                  title={`Filter by ${visitor.city}`}
                >
                  {visitor.city}
                </button>
                {(visitor.region || visitor.country) && (
                  <span className="text-muted-foreground">,</span>
                )}
              </>
            )}
            {visitor.region && visitor.region !== visitor.city && (
              <>
                <button
                  type="button"
                  onClick={(e) =>
                    handleFilter(e, 'filter_region', visitor.region)
                  }
                  className="hover:underline truncate"
                  title={`Filter by ${visitor.region}`}
                >
                  {visitor.region}
                </button>
                {visitor.country && (
                  <span className="text-muted-foreground">,</span>
                )}
              </>
            )}
            {visitor.country && (
              <button
                type="button"
                onClick={(e) =>
                  handleFilter(e, 'filter_country', visitor.country)
                }
                className="hover:underline truncate"
                title={`Filter by ${visitor.country}`}
              >
                {visitor.country}
              </button>
            )}
            {!visitor.city && !visitor.region && !visitor.country && (
              <span className="text-muted-foreground">Unknown</span>
            )}
          </div>
          {visitor.is_eu && (
            <Badge variant="outline" className="text-[10px] px-1.5 py-0">
              EU
            </Badge>
          )}
        </div>
      </TableCell>

      {/* Source / Referrer */}
      <TableCell className="hidden md:table-cell">
        <VisitorSource visitor={visitor} />
      </TableCell>

      {/* Browser / OS */}
      <TableCell className="hidden lg:table-cell">
        <div className="flex items-center gap-1.5">
          <span className="text-sm">
            {browserInfo?.name || 'Unknown'}
          </span>
          {osName && osName !== 'Unknown' && (
            <span className="text-xs text-muted-foreground">/ {osName}</span>
          )}
        </div>
      </TableCell>

      {/* First Seen */}
      <TableCell>
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="text-sm text-muted-foreground cursor-default">
              {formatDistanceToNow(firstSeenDate, { addSuffix: true })}
            </span>
          </TooltipTrigger>
          <TooltipContent>
            {format(firstSeenDate, 'MMM d, yyyy HH:mm:ss')}
          </TooltipContent>
        </Tooltip>
      </TableCell>

      {/* Last Seen */}
      <TableCell>
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="text-sm text-muted-foreground cursor-default">
              {formatDistanceToNow(lastSeenDate, { addSuffix: true })}
            </span>
          </TooltipTrigger>
          <TooltipContent>
            {format(lastSeenDate, 'MMM d, yyyy HH:mm:ss')}
          </TooltipContent>
        </Tooltip>
      </TableCell>
    </TableRow>
  )
}

function VisitorSource({ visitor }: { visitor: VisitorInfo }) {
  const channel = visitor.first_channel
  const hostname = visitor.first_referrer_hostname

  if (!channel && !hostname) {
    return <span className="text-sm text-muted-foreground">Direct</span>
  }

  return (
    <div className="flex flex-col gap-0.5">
      {channel && (
        <Badge variant="outline" className="text-[10px] px-1.5 py-0 w-fit">
          {channel}
        </Badge>
      )}
      {hostname && (
        <div className="flex items-center gap-1">
          <ExternalLink className="h-3 w-3 text-muted-foreground flex-shrink-0" />
          <span className="text-xs text-muted-foreground truncate max-w-[150px]">
            {hostname}
          </span>
        </div>
      )}
    </div>
  )
}

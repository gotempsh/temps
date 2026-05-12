import { getVisitorsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse, VisitorInfo } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useQuery } from '@tanstack/react-query'
import { format, formatDistanceToNow } from 'date-fns'
import { ArrowLeft, Bot, ChevronLeft, ChevronRight, Globe, User } from 'lucide-react'
import * as React from 'react'
import { useNavigate } from 'react-router-dom'
import type { DimensionKey } from './DimensionList'

const PAGE_SIZE = 25

/**
 * Map a dimension key to the matching backend query parameter on
 * `GET /analytics/visitors`. Returns `null` when the dimension isn't
 * filterable as a segment (e.g. `pages`).
 */
function segmentParamFor(dimension: DimensionKey): string | null {
  switch (dimension) {
    case 'events':
      return 'filter_event'
    case 'referrers':
      return 'filter_referrer'
    case 'browsers':
      return 'filter_browser'
    case 'operating_systems':
      return 'filter_os'
    case 'devices':
      return 'filter_device'
    case 'countries':
      return 'filter_country'
    case 'regions':
      return 'filter_region'
    case 'cities':
      return 'filter_city'
    case 'channels':
      return 'filter_channel'
    case 'languages':
      return 'filter_language'
    case 'utm_source':
      return 'filter_utm_source'
    case 'utm_medium':
      return 'filter_utm_medium'
    case 'utm_campaign':
      return 'filter_utm_campaign'
    case 'utm_term':
      return 'filter_utm_term'
    case 'utm_content':
      return 'filter_utm_content'
    case 'pages':
      return null
  }
}

export function segmentSupportsVisitors(dimension: DimensionKey): boolean {
  return segmentParamFor(dimension) !== null
}

const DIMENSION_LABEL: Record<DimensionKey, string> = {
  events: 'event',
  referrers: 'referrer',
  browsers: 'browser',
  operating_systems: 'operating system',
  devices: 'device',
  countries: 'country',
  regions: 'region',
  cities: 'city',
  channels: 'channel',
  languages: 'language',
  utm_source: 'UTM source',
  utm_medium: 'UTM medium',
  utm_campaign: 'UTM campaign',
  utm_term: 'UTM term',
  utm_content: 'UTM content',
  pages: 'page',
}

function countryCodeToFlag(countryCode: string | null | undefined): string {
  if (!countryCode || countryCode.length !== 2) return ''
  const codePoints = countryCode
    .toUpperCase()
    .split('')
    .map((char) => 127397 + char.charCodeAt(0))
  return String.fromCodePoint(...codePoints)
}

function formatLocation(visitor: VisitorInfo): string {
  const parts: string[] = []
  if (visitor.city) parts.push(visitor.city)
  if (visitor.region && visitor.region !== visitor.city) parts.push(visitor.region)
  if (visitor.country) parts.push(visitor.country)
  return parts.join(', ') || 'Unknown'
}

interface SegmentVisitorsProps {
  project: ProjectResponse
  dimension: DimensionKey
  value: string
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  onBack: () => void
}

export function SegmentVisitors({
  project,
  dimension,
  value,
  startDate,
  endDate,
  environment,
  onBack,
}: SegmentVisitorsProps) {
  const navigate = useNavigate()
  const [page, setPage] = React.useState(1)

  const filterParam = segmentParamFor(dimension)

  // Reset to page 1 whenever segment / date / env changes.
  React.useEffect(() => {
    setPage(1)
  }, [dimension, value, startDate, endDate, environment])

  const offset = (page - 1) * PAGE_SIZE
  const segmentQuery: Record<string, string | number | boolean | undefined> = {}
  if (filterParam) {
    segmentQuery[filterParam] = value
  }

  const { data, isLoading, error } = useQuery({
    ...getVisitorsOptions({
      query: {
        project_id: project.id,
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        include_crawlers: false,
        has_activity_only: true,
        limit: PAGE_SIZE,
        offset,
        ...segmentQuery,
      },
    }),
    enabled: !!startDate && !!endDate && !!filterParam,
  })

  const visitors = data?.visitors ?? []
  const totalCount = data?.filtered_count ?? data?.total_count ?? 0
  const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE))

  if (!filterParam) {
    return (
      <Card>
        <CardContent className="py-10 text-center">
          <p className="text-sm text-muted-foreground">
            {DIMENSION_LABEL[dimension]} segments aren&apos;t drillable to
            visitors.
          </p>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-2 min-w-0">
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7 shrink-0"
              onClick={onBack}
              aria-label="Back"
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="min-w-0">
              <CardTitle className="truncate">
                Visitors —{' '}
                <span className="text-muted-foreground font-normal">
                  {DIMENSION_LABEL[dimension]}:
                </span>{' '}
                <span className="font-mono">{value}</span>
              </CardTitle>
              <CardDescription>
                {startDate && endDate
                  ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                  : 'Select a date range'}
                {' · sorted by last action'}
              </CardDescription>
            </div>
          </div>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {isLoading ? (
          <VisitorsTableSkeleton />
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-12 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load visitors for this segment
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : visitors.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12 text-center">
            <p className="text-sm text-muted-foreground">
              No visitors match this segment in the selected period.
            </p>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Visitor</TableHead>
                  <TableHead>Last action</TableHead>
                  <TableHead className="hidden md:table-cell">Location</TableHead>
                  <TableHead className="hidden lg:table-cell">
                    Current page
                  </TableHead>
                  <TableHead className="hidden lg:table-cell">Channel</TableHead>
                  <TableHead className="hidden md:table-cell">First seen</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {visitors.map((visitor) => (
                  <TableRow
                    key={visitor.id}
                    className="cursor-pointer hover:bg-muted/50"
                    onClick={() =>
                      navigate(
                        `/projects/${project.slug}/analytics/visitors/${visitor.id}`,
                      )
                    }
                  >
                    <TableCell>
                      <div className="flex items-center gap-1.5">
                        {visitor.is_crawler ? (
                          <Bot className="h-3 w-3 text-muted-foreground shrink-0" />
                        ) : (
                          <User className="h-3 w-3 text-muted-foreground shrink-0" />
                        )}
                        <div className="flex flex-col leading-tight">
                          <span className="text-sm font-medium font-mono">
                            {visitor.visitor_id.slice(0, 8)}
                          </span>
                          <span className="text-xs text-muted-foreground">
                            #{visitor.id}
                          </span>
                        </div>
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-col leading-tight">
                        <span className="text-sm">
                          {formatDistanceToNow(new Date(visitor.last_seen), {
                            addSuffix: true,
                          })}
                        </span>
                        <span className="text-xs text-muted-foreground">
                          {format(new Date(visitor.last_seen), 'MMM d, HH:mm')}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <span className="text-sm inline-flex items-center gap-1.5">
                        {visitor.country_code ? (
                          <span aria-hidden>
                            {countryCodeToFlag(visitor.country_code)}
                          </span>
                        ) : (
                          <Globe className="h-3 w-3 text-muted-foreground shrink-0" />
                        )}
                        <span className="truncate max-w-[180px]">
                          {formatLocation(visitor)}
                        </span>
                      </span>
                    </TableCell>
                    <TableCell className="hidden lg:table-cell">
                      <span className="text-sm text-muted-foreground font-mono truncate block max-w-[200px]">
                        {visitor.current_page || '—'}
                      </span>
                    </TableCell>
                    <TableCell className="hidden lg:table-cell">
                      <span className="text-sm text-muted-foreground">
                        {visitor.first_channel || '—'}
                      </span>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <span className="text-sm text-muted-foreground">
                        {format(new Date(visitor.first_seen), 'MMM d, y')}
                      </span>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
      {!isLoading && !error && totalCount > 0 && (
        <CardFooter className="flex flex-col gap-2 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-sm text-muted-foreground">
            <span className="hidden sm:inline">
              Showing {offset + 1}–{Math.min(offset + visitors.length, totalCount)}{' '}
              of {totalCount.toLocaleString()} visitor
              {totalCount === 1 ? '' : 's'}
            </span>
            <span className="sm:hidden">
              Page {page} / {totalPages}
            </span>
          </p>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
            >
              <ChevronLeft className="h-4 w-4" />
              <span className="hidden sm:inline">Previous</span>
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={page >= totalPages}
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
            >
              <span className="hidden sm:inline">Next</span>
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

function VisitorsTableSkeleton() {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Visitor</TableHead>
          <TableHead>Last action</TableHead>
          <TableHead className="hidden md:table-cell">Location</TableHead>
          <TableHead className="hidden lg:table-cell">Current page</TableHead>
          <TableHead className="hidden lg:table-cell">Channel</TableHead>
          <TableHead className="hidden md:table-cell">First seen</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'].map((key) => (
          <TableRow key={`sv-skel-${key}`}>
            <TableCell>
              <div className="flex items-center gap-1.5">
                <Skeleton className="h-3 w-3 rounded-full" />
                <div className="flex flex-col gap-1">
                  <Skeleton className="h-4 w-16" />
                  <Skeleton className="h-3 w-10" />
                </div>
              </div>
            </TableCell>
            <TableCell>
              <div className="flex flex-col gap-1">
                <Skeleton className="h-4 w-20" />
                <Skeleton className="h-3 w-24" />
              </div>
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-4 w-32" />
            </TableCell>
            <TableCell className="hidden lg:table-cell">
              <Skeleton className="h-4 w-40" />
            </TableCell>
            <TableCell className="hidden lg:table-cell">
              <Skeleton className="h-4 w-20" />
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-4 w-20" />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

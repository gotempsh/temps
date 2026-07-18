import {
  getEventDetailOptions,
  getEventEntriesOptions,
  getEventVisitorsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { EventEntryInfo, ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import { Input } from '@/components/ui/input'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
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
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { cn } from '@/lib/utils'
import {
  AppWindow,
  ArrowLeft,
  BarChart3,
  Check,
  ChevronDown,
  ChevronRight,
  Columns3,
  Globe,
  Hash,
  Link2,
  Loader2,
  Plus,
  Users,
  X,
} from 'lucide-react'
import { Fragment, useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { TimeAgo } from '../utils/TimeAgo'

interface EventDetailProps {
  project: ProjectResponse
  eventName: string
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
  onBack: () => void
}

export function EventDetail({
  project,
  eventName,
  startDate,
  endDate,
  environment,
  onBack,
}: EventDetailProps) {
  const navigate = useNavigate()
  const [currentPage, setCurrentPage] = useState(1)
  const perPage = 20

  // Fetch event detail analytics
  const { data: detailData, isLoading: detailLoading } = useQuery({
    ...getEventDetailOptions({
      query: {
        event_name: eventName,
        project_id: project.id,
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  // Fetch visitors for this event
  const { data: visitorsData, isLoading: visitorsLoading } = useQuery({
    ...getEventVisitorsOptions({
      query: {
        event_name: eventName,
        project_id: project.id,
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        page: currentPage,
        per_page: perPage,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const totalPages = visitorsData
    ? Math.ceil(visitorsData.total_count / perPage)
    : 0

  return (
    <div className="space-y-6">
      {/* Back button */}
      <div className="flex items-center gap-3">
        <Button variant="ghost" size="sm" onClick={onBack} className="gap-2">
          <ArrowLeft className="h-4 w-4" />
          Back
        </Button>
      </div>

      {/* Event name title */}
      <div>
        <h2 className="text-2xl font-bold font-mono">{eventName}</h2>
        <p className="text-sm text-muted-foreground mt-1">
          {startDate && endDate
            ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
            : 'Event analytics'}
        </p>
      </div>

      {/* Summary stats */}
      {detailLoading ? (
        <div className="grid grid-cols-2 md:grid-cols-3 gap-4">
          {['total', 'visitors', 'sessions'].map((key) => (
            <Card key={`stat-skeleton-${key}`}>
              <CardContent className="pt-4 pb-4">
                <div className="flex items-center gap-2 mb-1">
                  <Skeleton className="h-4 w-4 rounded" />
                  <Skeleton className="h-3 w-16" />
                </div>
                <Skeleton className="h-7 w-14" />
              </CardContent>
            </Card>
          ))}
        </div>
      ) : detailData ? (
        <div className="grid grid-cols-2 md:grid-cols-3 gap-4">
          <StatCard
            label="Total Count"
            value={detailData.total_count.toLocaleString()}
            icon={<Hash className="h-4 w-4 text-muted-foreground" />}
          />
          <StatCard
            label="Unique Visitors"
            value={detailData.unique_visitors.toLocaleString()}
            icon={<Users className="h-4 w-4 text-muted-foreground" />}
          />
          <StatCard
            label="Unique Sessions"
            value={detailData.unique_sessions.toLocaleString()}
            icon={<BarChart3 className="h-4 w-4 text-muted-foreground" />}
          />
        </div>
      ) : null}

      {/* Referrers, Countries, Browsers side by side */}
      {detailLoading && (
        <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
          {['referrers', 'countries', 'browsers'].map((section) => (
            <Card key={`breakdown-skeleton-${section}`}>
              <CardHeader className="pb-3">
                <Skeleton className="h-4 w-24" />
              </CardHeader>
              <CardContent className="pt-0">
                <div className="space-y-2">
                  {['a', 'b', 'c', 'd'].map((row) => (
                    <div
                      key={`row-${section}-${row}`}
                      className="flex items-center justify-between"
                    >
                      <Skeleton className="h-3 w-20" />
                      <div className="flex items-center gap-2">
                        <Skeleton className="h-3 w-8" />
                        <Skeleton className="h-5 w-12 rounded-full" />
                      </div>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
      {detailData && (
        <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
          {/* Top Referrers */}
          <BreakdownCard
            title="Top Referrers"
            icon={<Link2 className="h-4 w-4 text-muted-foreground" />}
            items={detailData.referrers.slice(0, 8)}
            renderItem={(ref) => ({
              label: ref.referrer || '(direct)',
              count: ref.count,
              percentage: ref.percentage,
            })}
            emptyMessage="No referrer data"
          />

          {/* Top Countries */}
          <BreakdownCard
            title="Top Countries"
            icon={<Globe className="h-4 w-4 text-muted-foreground" />}
            items={detailData.countries.slice(0, 8)}
            renderItem={(country) => ({
              label: country.country,
              count: country.count,
              percentage: country.percentage,
            })}
            emptyMessage="No location data"
          />

          {/* Top Browsers */}
          <BreakdownCard
            title="Top Browsers"
            icon={<AppWindow className="h-4 w-4 text-muted-foreground" />}
            items={detailData.browsers.slice(0, 8)}
            renderItem={(browser) => ({
              label: browser.browser,
              count: browser.count,
              percentage: browser.percentage,
            })}
            emptyMessage="No browser data"
          />
        </div>
      )}

      {/* Individual event occurrences with custom data */}
      <EventEntriesCard
        key={eventName}
        project={project}
        eventName={eventName}
        startDate={startDate}
        endDate={endDate}
        environment={environment}
      />

      {/* Visitors Table */}
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle className="text-base">Visitors</CardTitle>
              <CardDescription>
                Visitors who triggered this event
                {visitorsData && (
                  <span className="ml-1">
                    ({visitorsData.total_count.toLocaleString()} unique)
                  </span>
                )}
              </CardDescription>
            </div>
            {visitorsLoading && (
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                Loading...
              </div>
            )}
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {visitorsLoading && !visitorsData ? (
            <VisitorsTableSkeleton />
          ) : !visitorsData?.visitors || visitorsData.visitors.length === 0 ? (
            <div className="p-8 text-center">
              <p className="text-sm text-muted-foreground">
                No visitors found for this event in the selected date range
              </p>
            </div>
          ) : (
            <>
              <div className="overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Visitor</TableHead>
                      <TableHead className="text-right">Count</TableHead>
                      <TableHead>First → Last</TableHead>
                      <TableHead className="hidden md:table-cell">
                        Device
                      </TableHead>
                      <TableHead className="hidden lg:table-cell">
                        Browser
                      </TableHead>
                      <TableHead className="hidden md:table-cell">
                        Location
                      </TableHead>
                      <TableHead className="hidden lg:table-cell">
                        Referrer
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {visitorsData.visitors.map((visitor) => (
                      <TableRow
                        key={visitor.visitor_id}
                        className="cursor-pointer hover:bg-muted/50"
                        onClick={() =>
                          navigate(
                            `/projects/${project.slug}/analytics/visitors/${visitor.visitor_id}`
                          )
                        }
                      >
                        <TableCell>
                          <div className="flex items-center gap-1.5">
                            <Users className="h-3 w-3 text-muted-foreground shrink-0" />
                            <div className="flex flex-col">
                              <span className="text-sm font-medium font-mono">
                                {visitor.visitor_uuid?.slice(0, 8) ||
                                  visitor.visitor_id}
                              </span>
                              <span className="text-xs text-muted-foreground">
                                #{visitor.visitor_id}
                              </span>
                            </div>
                          </div>
                        </TableCell>
                        <TableCell className="text-right">
                          <Badge variant="secondary" className="text-xs">
                            {visitor.event_count.toLocaleString()}×
                          </Badge>
                        </TableCell>
                        <TableCell>
                          <div className="flex flex-col leading-tight">
                            <span className="text-sm">
                              <TimeAgo date={visitor.last_triggered} />
                            </span>
                            {visitor.first_triggered !==
                              visitor.last_triggered && (
                              <span className="text-xs text-muted-foreground">
                                first{' '}
                                {format(
                                  new Date(visitor.first_triggered),
                                  'MMM d, HH:mm'
                                )}
                              </span>
                            )}
                          </div>
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          <span className="text-sm text-muted-foreground">
                            {visitor.device_type || '-'}
                          </span>
                        </TableCell>
                        <TableCell className="hidden lg:table-cell">
                          <span className="text-sm text-muted-foreground">
                            {visitor.browser || '-'}
                          </span>
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          <VisitorLocation visitor={visitor} />
                        </TableCell>
                        <TableCell className="hidden lg:table-cell">
                          <span className="text-sm text-muted-foreground truncate max-w-[150px] block">
                            {visitor.referrer_hostname || 'Direct'}
                          </span>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>

              {/* Pagination */}
              {totalPages > 1 && (
                <div className="flex items-center justify-between px-4 py-3 border-t">
                  <p className="text-sm text-muted-foreground">
                    Page {currentPage} of {totalPages}
                  </p>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={currentPage <= 1}
                      onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
                    >
                      Previous
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={currentPage >= totalPages}
                      onClick={() =>
                        setCurrentPage((p) => Math.min(totalPages, p + 1))
                      }
                    >
                      Next
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

// ============================================================================
// Helper Components
// ============================================================================

interface EventEntriesCardProps {
  project: ProjectResponse
  eventName: string
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

function EventEntriesCard({
  project,
  eventName,
  startDate,
  endDate,
  environment,
}: EventEntriesCardProps) {
  const navigate = useNavigate()
  const [currentPage, setCurrentPage] = useState(1)
  const [expandedId, setExpandedId] = useState<number | null>(null)
  const perPage = 20

  const columnsStorageKey = `temps-event-prop-columns:${project.id}:${eventName}`
  const [propColumns, setPropColumnsState] = useState<string[]>(() =>
    loadStoredPropColumns(columnsStorageKey)
  )
  const setPropColumns = (columns: string[]) => {
    setPropColumnsState(columns)
    try {
      localStorage.setItem(columnsStorageKey, JSON.stringify(columns))
    } catch {
      // Persistence is best-effort; the in-memory state still applies
    }
  }

  const { data: entriesData, isLoading: entriesLoading } = useQuery({
    ...getEventEntriesOptions({
      query: {
        event_name: eventName,
        project_id: project.id,
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        page: currentPage,
        per_page: perPage,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const totalPages = entriesData
    ? Math.ceil(entriesData.total_count / perPage)
    : 0

  const toggleExpanded = (entry: EventEntryInfo) => {
    if (!entry.props) return
    setExpandedId((current) => (current === entry.id ? null : entry.id))
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="text-base">Events</CardTitle>
            <CardDescription>
              Individual occurrences of this event
              {entriesData && (
                <span className="ml-1">
                  ({entriesData.total_count.toLocaleString()} total)
                </span>
              )}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            {entriesLoading && entriesData && (
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                Loading...
              </div>
            )}
            <PropColumnsPicker
              entries={entriesData?.entries ?? []}
              columns={propColumns}
              onChange={setPropColumns}
            />
          </div>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {entriesLoading && !entriesData ? (
          <EntriesTableSkeleton />
        ) : !entriesData?.entries || entriesData.entries.length === 0 ? (
          <div className="p-8 text-center">
            <p className="text-sm text-muted-foreground">
              No events found in the selected date range
            </p>
          </div>
        ) : (
          <>
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-8" />
                    <TableHead>Time</TableHead>
                    <TableHead>Visitor</TableHead>
                    <TableHead className="hidden md:table-cell">Page</TableHead>
                    {propColumns.map((col) => (
                      <TableHead key={col} className="hidden md:table-cell">
                        <div className="flex items-center gap-1">
                          <span className="max-w-[140px] truncate font-mono text-xs">
                            {col}
                          </span>
                          <button
                            type="button"
                            aria-label={`Remove ${col} column`}
                            className="text-muted-foreground/50 transition-colors hover:text-foreground"
                            onClick={() =>
                              setPropColumns(
                                propColumns.filter((c) => c !== col)
                              )
                            }
                          >
                            <X className="h-3 w-3" />
                          </button>
                        </div>
                      </TableHead>
                    ))}
                    <TableHead className="hidden sm:table-cell">Data</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {entriesData.entries.map((entry) => (
                    <Fragment key={entry.id}>
                      <TableRow
                        className={
                          entry.props
                            ? 'cursor-pointer hover:bg-muted/50'
                            : undefined
                        }
                        onClick={() => toggleExpanded(entry)}
                      >
                        <TableCell className="pr-0">
                          {entry.props ? (
                            expandedId === entry.id ? (
                              <ChevronDown className="h-4 w-4 text-muted-foreground" />
                            ) : (
                              <ChevronRight className="h-4 w-4 text-muted-foreground" />
                            )
                          ) : null}
                        </TableCell>
                        <TableCell>
                          <div className="flex flex-col leading-tight">
                            <span className="text-sm">
                              <TimeAgo date={entry.timestamp} />
                            </span>
                            <span className="text-xs text-muted-foreground">
                              {format(
                                new Date(entry.timestamp),
                                'MMM d, HH:mm:ss'
                              )}
                            </span>
                          </div>
                        </TableCell>
                        <TableCell>
                          {entry.visitor_id ? (
                            <button
                              type="button"
                              className="flex items-center gap-1.5 hover:underline"
                              onClick={(e) => {
                                e.stopPropagation()
                                navigate(
                                  `/projects/${project.slug}/analytics/visitors/${entry.visitor_id}`
                                )
                              }}
                            >
                              <Users className="h-3 w-3 text-muted-foreground shrink-0" />
                              <span className="text-sm font-mono">
                                {entry.visitor_uuid?.slice(0, 8) ||
                                  entry.visitor_id}
                              </span>
                            </button>
                          ) : (
                            <span className="text-sm text-muted-foreground">
                              -
                            </span>
                          )}
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          <span className="text-sm text-muted-foreground truncate max-w-[200px] block">
                            {entry.page_path}
                          </span>
                        </TableCell>
                        {propColumns.map((col) => (
                          <TableCell key={col} className="hidden md:table-cell">
                            <PropValueCell
                              value={
                                entry.props
                                  ? resolvePropPath(entry.props, col)
                                  : undefined
                              }
                            />
                          </TableCell>
                        ))}
                        <TableCell className="hidden sm:table-cell">
                          {entry.props ? (
                            <span className="text-xs font-mono text-muted-foreground truncate max-w-[280px] block">
                              {formatPropsPreview(entry.props)}
                            </span>
                          ) : (
                            <span className="text-xs text-muted-foreground">
                              -
                            </span>
                          )}
                        </TableCell>
                      </TableRow>
                      {expandedId === entry.id && entry.props && (
                        <TableRow className="hover:bg-transparent">
                          <TableCell
                            colSpan={5 + propColumns.length}
                            className="bg-muted/30 p-0"
                          >
                            <CodeBlock
                              code={JSON.stringify(entry.props, null, 2)}
                              language="json"
                              title="Custom data"
                              className="m-3"
                            />
                          </TableCell>
                        </TableRow>
                      )}
                    </Fragment>
                  ))}
                </TableBody>
              </Table>
            </div>

            {/* Pagination */}
            {totalPages > 1 && (
              <div className="flex items-center justify-between px-4 py-3 border-t">
                <p className="text-sm text-muted-foreground">
                  Page {currentPage} of {totalPages}
                </p>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={currentPage <= 1}
                    onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
                  >
                    Previous
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={currentPage >= totalPages}
                    onClick={() =>
                      setCurrentPage((p) => Math.min(totalPages, p + 1))
                    }
                  >
                    Next
                  </Button>
                </div>
              </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}

function formatPropsPreview(props: Record<string, unknown>): string {
  const compact = JSON.stringify(props)
  return compact.length > 80 ? `${compact.slice(0, 80)}…` : compact
}

// ============================================================================
// Property Columns (pin JSON props as table columns)
// ============================================================================

const MAX_PROP_COLUMNS = 4

/** Split a path like `items[0].sku` into segments `['items', '0', 'sku']` */
function parsePropPath(path: string): string[] {
  return path
    .replace(/\[(\d+)\]/g, '.$1')
    .split('.')
    .filter(Boolean)
}

function resolvePropPath(
  props: Record<string, unknown>,
  path: string
): unknown {
  let current: unknown = props
  for (const segment of parsePropPath(path)) {
    if (Array.isArray(current)) {
      current = current[Number(segment)]
    } else if (current && typeof current === 'object') {
      current = (current as Record<string, unknown>)[segment]
    } else {
      return undefined
    }
  }
  return current
}

function formatPropValue(value: unknown): string {
  if (value === undefined) return '-'
  if (value === null) return 'null'
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value)
  }
  const compact = JSON.stringify(value)
  return compact.length > 40 ? `${compact.slice(0, 40)}…` : compact
}

interface DiscoveredPath {
  path: string
  sample: unknown
  count: number
}

/**
 * Walk the props of the loaded entries and collect every reachable path
 * (nested objects via dot notation, arrays via their first element) so the
 * picker can offer the actual structure of the data on screen.
 */
function discoverPropPaths(entries: EventEntryInfo[]): DiscoveredPath[] {
  const found = new Map<string, { sample: unknown; count: number }>()
  const maxDepth = 4

  const record = (path: string, value: unknown) => {
    const existing = found.get(path)
    if (existing) {
      existing.count += 1
      if (existing.sample === undefined || existing.sample === null) {
        existing.sample = value
      }
    } else {
      found.set(path, { sample: value, count: 1 })
    }
  }

  const visit = (value: unknown, prefix: string, depth: number) => {
    if (depth >= maxDepth || value === null || typeof value !== 'object') {
      return
    }
    if (Array.isArray(value)) {
      if (value.length > 0) {
        const path = `${prefix}[0]`
        record(path, value[0])
        visit(value[0], path, depth + 1)
      }
      return
    }
    for (const [key, child] of Object.entries(value)) {
      const path = prefix ? `${prefix}.${key}` : key
      record(path, child)
      visit(child, path, depth + 1)
    }
  }

  for (const entry of entries) {
    if (entry.props) visit(entry.props, '', 0)
  }

  return Array.from(found.entries())
    .map(([path, { sample, count }]) => ({ path, sample, count }))
    .sort((a, b) => b.count - a.count || a.path.localeCompare(b.path))
}

function loadStoredPropColumns(key: string): string[] {
  try {
    const raw = localStorage.getItem(key)
    const parsed: unknown = raw ? JSON.parse(raw) : []
    if (!Array.isArray(parsed)) return []
    return parsed
      .filter((c): c is string => typeof c === 'string' && c.length > 0)
      .slice(0, MAX_PROP_COLUMNS)
  } catch {
    return []
  }
}

interface PropColumnsPickerProps {
  entries: EventEntryInfo[]
  columns: string[]
  onChange: (columns: string[]) => void
}

function PropColumnsPicker({
  entries,
  columns,
  onChange,
}: PropColumnsPickerProps) {
  const [open, setOpen] = useState(false)
  const [manualPath, setManualPath] = useState('')

  const discovered = useMemo(() => discoverPropPaths(entries), [entries])
  const atLimit = columns.length >= MAX_PROP_COLUMNS

  const toggle = (path: string) => {
    if (columns.includes(path)) {
      onChange(columns.filter((c) => c !== path))
    } else if (!atLimit) {
      onChange([...columns, path])
    }
  }

  const manualTrimmed = manualPath.trim()
  const canAddManual =
    manualTrimmed.length > 0 && !columns.includes(manualTrimmed) && !atLimit

  const addManual = () => {
    if (!canAddManual) return
    onChange([...columns, manualTrimmed])
    setManualPath('')
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm" className="gap-1.5">
          <Columns3 className="h-4 w-4" />
          <span className="hidden sm:inline">Columns</span>
          {columns.length > 0 && (
            <Badge variant="secondary" className="px-1.5 text-xs">
              {columns.length}
            </Badge>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-0">
        <div className="border-b px-3 py-2.5">
          <p className="text-sm font-medium">Property columns</p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Pin up to {MAX_PROP_COLUMNS} properties from the custom data as
            columns
          </p>
        </div>
        <Command>
          <CommandInput placeholder="Search properties..." />
          <CommandList className="max-h-52">
            <CommandEmpty>
              {discovered.length === 0
                ? 'No custom data in the events on this page'
                : 'No matching property'}
            </CommandEmpty>
            <CommandGroup>
              {discovered.map((item) => {
                const checked = columns.includes(item.path)
                return (
                  <CommandItem
                    key={item.path}
                    value={item.path}
                    disabled={!checked && atLimit}
                    onSelect={() => toggle(item.path)}
                  >
                    <Check
                      className={cn(
                        'mr-2 h-4 w-4 shrink-0',
                        checked ? 'opacity-100' : 'opacity-20'
                      )}
                    />
                    <span className="truncate font-mono text-xs">
                      {item.path}
                    </span>
                    <span className="ml-auto max-w-[110px] truncate pl-2 text-xs text-muted-foreground">
                      {formatPropValue(item.sample)}
                    </span>
                  </CommandItem>
                )
              })}
            </CommandGroup>
          </CommandList>
        </Command>
        <div className="border-t p-2">
          <p className="mb-1.5 px-1 text-xs text-muted-foreground">
            Or add a path manually
          </p>
          <div className="flex items-center gap-1.5">
            <Input
              value={manualPath}
              onChange={(e) => setManualPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  addManual()
                }
              }}
              placeholder="e.g. items[0].sku"
              className="h-8 font-mono text-xs"
            />
            <Button
              size="sm"
              variant="secondary"
              className="h-8 px-2"
              onClick={addManual}
              disabled={!canAddManual}
              aria-label="Add property column"
            >
              <Plus className="h-4 w-4" />
            </Button>
          </div>
          {atLimit && (
            <p className="mt-1.5 px-1 text-xs text-muted-foreground">
              Column limit reached — remove one to add another
            </p>
          )}
        </div>
      </PopoverContent>
    </Popover>
  )
}

function PropValueCell({ value }: { value: unknown }) {
  if (value === undefined) {
    return <span className="text-xs text-muted-foreground">-</span>
  }
  if (typeof value === 'number') {
    return (
      <span className="font-mono text-sm tabular-nums">{String(value)}</span>
    )
  }
  if (typeof value === 'boolean' || value === null) {
    return (
      <span className="font-mono text-sm text-muted-foreground">
        {String(value)}
      </span>
    )
  }
  if (typeof value === 'string') {
    return <span className="block max-w-[160px] truncate text-sm">{value}</span>
  }
  return (
    <span className="block max-w-[160px] truncate font-mono text-xs text-muted-foreground">
      {JSON.stringify(value)}
    </span>
  )
}

function EntriesTableSkeleton() {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead className="w-8" />
          <TableHead>Time</TableHead>
          <TableHead>Visitor</TableHead>
          <TableHead className="hidden md:table-cell">Page</TableHead>
          <TableHead className="hidden sm:table-cell">Data</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {['s1', 's2', 's3', 's4', 's5'].map((key) => (
          <TableRow key={`entry-skeleton-${key}`}>
            <TableCell className="pr-0">
              <Skeleton className="h-4 w-4" />
            </TableCell>
            <TableCell>
              <div className="flex flex-col gap-1">
                <Skeleton className="h-4 w-20" />
                <Skeleton className="h-3 w-24" />
              </div>
            </TableCell>
            <TableCell>
              <Skeleton className="h-4 w-16" />
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-4 w-28" />
            </TableCell>
            <TableCell className="hidden sm:table-cell">
              <Skeleton className="h-4 w-40" />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

interface StatCardProps {
  label: string
  value: string
  icon: React.ReactNode
}

function StatCard({ label, value, icon }: StatCardProps) {
  return (
    <Card>
      <CardContent className="pt-4 pb-4">
        <div className="flex items-center gap-2 mb-1">
          {icon}
          <span className="text-xs text-muted-foreground">{label}</span>
        </div>
        <p className="text-lg font-semibold">{value}</p>
      </CardContent>
    </Card>
  )
}

interface BreakdownItem {
  label: string
  count: number
  percentage: number
}

interface BreakdownCardProps<T> {
  title: string
  icon: React.ReactNode
  items: T[]
  renderItem: (item: T) => BreakdownItem
  emptyMessage: string
}

function BreakdownCard<T>({
  title,
  icon,
  items,
  renderItem,
  emptyMessage,
}: BreakdownCardProps<T>) {
  if (items.length === 0) {
    return (
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-sm font-medium flex items-center gap-2">
            {icon}
            {title}
          </CardTitle>
        </CardHeader>
        <CardContent className="pt-0">
          <p className="text-sm text-muted-foreground">{emptyMessage}</p>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-sm font-medium flex items-center gap-2">
          {icon}
          {title}
        </CardTitle>
      </CardHeader>
      <CardContent className="pt-0">
        <div className="space-y-2">
          {items.map((item) => {
            const { label, count, percentage } = renderItem(item)
            return (
              <div
                key={`${label}-${count}`}
                className="flex items-center justify-between text-sm"
              >
                <span className="truncate text-muted-foreground max-w-[60%]">
                  {label}
                </span>
                <div className="flex items-center gap-2">
                  <span className="font-medium">{count}</span>
                  <Badge variant="outline" className="text-xs">
                    {percentage.toFixed(1)}%
                  </Badge>
                </div>
              </div>
            )
          })}
        </div>
      </CardContent>
    </Card>
  )
}

interface VisitorLocationProps {
  visitor: {
    city?: string | null
    country?: string | null
    country_code?: string | null
  }
}

function VisitorLocation({ visitor }: VisitorLocationProps) {
  const parts: string[] = []
  if (visitor.city) parts.push(visitor.city)
  if (visitor.country) parts.push(visitor.country)

  if (parts.length === 0) {
    return <span className="text-xs text-muted-foreground">-</span>
  }

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div className="flex items-center gap-1.5">
          <Globe className="h-3 w-3 text-muted-foreground shrink-0" />
          <span className="text-sm truncate max-w-[120px]">
            {parts.join(', ')}
          </span>
        </div>
      </TooltipTrigger>
      <TooltipContent>
        <div className="text-xs">
          {visitor.city && <div>City: {visitor.city}</div>}
          {visitor.country && <div>Country: {visitor.country}</div>}
        </div>
      </TooltipContent>
    </Tooltip>
  )
}

function VisitorsTableSkeleton() {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Visitor</TableHead>
          <TableHead className="text-right">Count</TableHead>
          <TableHead>First → Last</TableHead>
          <TableHead className="hidden md:table-cell">Device</TableHead>
          <TableHead className="hidden lg:table-cell">Browser</TableHead>
          <TableHead className="hidden md:table-cell">Location</TableHead>
          <TableHead className="hidden lg:table-cell">Referrer</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {['s1', 's2', 's3', 's4', 's5'].map((key) => (
          <TableRow key={`visitor-skeleton-${key}`}>
            <TableCell>
              <div className="flex items-center gap-1.5">
                <Skeleton className="h-3 w-3 rounded-full" />
                <div className="flex flex-col gap-1">
                  <Skeleton className="h-4 w-16" />
                  <Skeleton className="h-3 w-10" />
                </div>
              </div>
            </TableCell>
            <TableCell className="text-right">
              <Skeleton className="h-5 w-8 rounded-full ml-auto" />
            </TableCell>
            <TableCell>
              <div className="flex flex-col gap-1">
                <Skeleton className="h-4 w-20" />
                <Skeleton className="h-3 w-24" />
              </div>
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-4 w-16" />
            </TableCell>
            <TableCell className="hidden lg:table-cell">
              <Skeleton className="h-4 w-16" />
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-4 w-20" />
            </TableCell>
            <TableCell className="hidden lg:table-cell">
              <Skeleton className="h-4 w-24" />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

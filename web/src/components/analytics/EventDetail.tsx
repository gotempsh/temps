import {
  getEventDetailOptions,
  getEventVisitorsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import {
  AppWindow,
  ArrowLeft,
  BarChart3,
  Globe,
  Hash,
  Link2,
  Loader2,
  Users,
} from 'lucide-react'
import { useState } from 'react'
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
          ) : !visitorsData?.visitors ||
            visitorsData.visitors.length === 0 ? (
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
                                first {format(
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
                      onClick={() =>
                        setCurrentPage((p) => Math.max(1, p - 1))
                      }
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

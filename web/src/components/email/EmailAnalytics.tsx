'use client'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { Progress } from '@/components/ui/progress'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
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
import { formatDistanceToNow } from 'date-fns'
import {
  AlertTriangle,
  BarChart3,
  ChevronLeft,
  ChevronRight,
  Eye,
  Globe,
  MailWarning,
  Monitor,
  MousePointerClick,
  Send,
  TrendingUp,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { EventBadge } from './shared'
import { parseUserAgent } from './sharedUtils'

// Types
interface EmailEventStats {
  delivered: number
  opened: number
  clicked: number
  bounced: number
  complained: number
  open_rate: number | null
  click_rate: number | null
  bounce_rate: number | null
}

interface EmailEvent {
  id: number
  email_id: string
  event_type: string
  provider_message_id: string | null
  recipient: string | null
  metadata: Record<string, unknown> | null
  ip_address: string | null
  user_agent: string | null
  created_at: string
}

interface PaginatedEmailEvents {
  events: EmailEvent[]
  total: number
  page: number
  page_size: number
}

// API calls
async function fetchEventStats(emailId?: string): Promise<EmailEventStats> {
  const url = emailId
    ? `/api/emails/events/stats?email_id=${emailId}`
    : '/api/emails/events/stats'
  const response = await fetch(url)
  if (!response.ok) throw new Error('Failed to fetch event stats')
  return response.json()
}

async function fetchAllEvents(params: {
  email_id?: string
  event_type?: string
  page?: number
  page_size?: number
}): Promise<PaginatedEmailEvents> {
  const searchParams = new URLSearchParams()
  if (params.email_id) searchParams.set('email_id', params.email_id)
  if (params.event_type) searchParams.set('event_type', params.event_type)
  if (params.page) searchParams.set('page', params.page.toString())
  if (params.page_size) searchParams.set('page_size', params.page_size.toString())

  const response = await fetch(`/api/emails/events?${searchParams}`)
  if (!response.ok) throw new Error('Failed to fetch events')
  return response.json()
}

function RateCard({
  title,
  count,
  rate,
  icon: Icon,
  color,
}: {
  title: string
  count: number
  rate: number | null
  icon: React.ComponentType<{ className?: string }>
  color: string
}) {
  const percentage = rate != null ? Math.round(rate) : null

  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-sm font-medium">{title}</CardTitle>
        <Icon className={`h-4 w-4 ${color}`} />
      </CardHeader>
      <CardContent>
        <div className="text-2xl font-bold">{count.toLocaleString()}</div>
        {percentage != null && (
          <div className="flex items-center gap-2 mt-2">
            <Progress value={percentage} className="h-1.5" />
            <span className="text-xs text-muted-foreground font-medium w-10 text-right">
              {percentage}%
            </span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

export function EmailAnalytics() {
  const navigate = useNavigate()
  const [eventType, setEventType] = useState<string | undefined>()
  const [page, setPage] = useState(1)
  const pageSize = 25

  const { data: stats, isLoading: isLoadingStats } = useQuery({
    queryKey: ['email-event-stats'],
    queryFn: () => fetchEventStats(),
  })

  const { data: events, isLoading: isLoadingEvents } = useQuery({
    queryKey: ['all-email-events', eventType, page],
    queryFn: () =>
      fetchAllEvents({
        event_type: eventType,
        page,
        page_size: pageSize,
      }),
  })

  const totalPages = events ? Math.ceil(events.total / pageSize) : 0

  if (isLoadingStats && isLoadingEvents) {
    return (
      <div className="space-y-6">
        <div className="grid gap-4 grid-cols-2 md:grid-cols-5">
          {[1, 2, 3, 4, 5].map((i) => (
            <Card key={i}>
              <CardHeader className="pb-2">
                <Skeleton className="h-4 w-16" />
              </CardHeader>
              <CardContent>
                <Skeleton className="h-8 w-12" />
                <Skeleton className="h-1.5 w-full mt-3" />
              </CardContent>
            </Card>
          ))}
        </div>
        <Skeleton className="h-10 w-full" />
        <div className="space-y-2">
          {[1, 2, 3, 4, 5].map((i) => (
            <Skeleton key={i} className="h-12 w-full" />
          ))}
        </div>
      </div>
    )
  }

  const hasEvents = stats && (stats.delivered > 0 || stats.opened > 0 || stats.clicked > 0 || stats.bounced > 0 || stats.complained > 0)

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold tracking-tight">Email Analytics</h2>
        <p className="text-muted-foreground">
          Track delivery, engagement, and bounce rates across all emails.
        </p>
      </div>

      {/* Stats Cards */}
      {stats && (
        <div className="grid gap-4 grid-cols-2 md:grid-cols-5">
          <RateCard
            title="Delivered"
            count={stats.delivered}
            rate={null}
            icon={Send}
            color="text-emerald-500"
          />
          <RateCard
            title="Opened"
            count={stats.opened}
            rate={stats.open_rate}
            icon={Eye}
            color="text-blue-500"
          />
          <RateCard
            title="Clicked"
            count={stats.clicked}
            rate={stats.click_rate}
            icon={MousePointerClick}
            color="text-green-500"
          />
          <RateCard
            title="Bounced"
            count={stats.bounced}
            rate={stats.bounce_rate}
            icon={MailWarning}
            color="text-red-500"
          />
          <RateCard
            title="Complained"
            count={stats.complained}
            rate={null}
            icon={AlertTriangle}
            color="text-orange-500"
          />
        </div>
      )}

      {/* Event Log */}
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between flex-wrap gap-2">
            <CardTitle className="text-base flex items-center gap-2">
              <BarChart3 className="h-4 w-4" />
              Event Log
              {events && events.total > 0 && (
                <Badge variant="secondary" className="text-xs font-normal">
                  {events.total.toLocaleString()}
                </Badge>
              )}
            </CardTitle>
            <Select
              value={eventType ?? 'all'}
              onValueChange={(v) => {
                setEventType(v === 'all' ? undefined : v)
                setPage(1)
              }}
            >
              <SelectTrigger className="w-[160px] h-8 text-xs">
                <SelectValue placeholder="All events" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All events</SelectItem>
                <SelectItem value="delivered">Delivered</SelectItem>
                {/* Stored event_type values are "open"/"click" — see
                    tracking_service.rs record_open/record_click. The endpoint
                    filters by exact match, so these values must match storage. */}
                <SelectItem value="open">Opened</SelectItem>
                <SelectItem value="click">Clicked</SelectItem>
                <SelectItem value="bounced">Bounced</SelectItem>
                <SelectItem value="complained">Complained</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </CardHeader>
        <CardContent>
          {!hasEvents && !events?.events.length ? (
            <EmptyState
              icon={TrendingUp}
              title="No events yet"
              description="Send emails with tracking enabled to start seeing analytics here."
            />
          ) : (
            <>
              <div className="rounded-md border overflow-x-auto">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Event</TableHead>
                      <TableHead className="hidden md:table-cell">Recipient</TableHead>
                      <TableHead className="hidden md:table-cell">Details</TableHead>
                      <TableHead className="hidden lg:table-cell">Source</TableHead>
                      <TableHead>Time</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {events?.events.map((event) => (
                      <TableRow
                        key={event.id}
                        className="cursor-pointer hover:bg-muted/50"
                        onClick={() => navigate(`/email/${event.email_id}`)}
                      >
                        <TableCell>
                          <EventBadge type={event.event_type} iconClassName="h-3.5 w-3.5" />
                        </TableCell>
                        <TableCell className="hidden md:table-cell">
                          {event.recipient ? (
                            <span className="text-sm truncate max-w-[200px] block">
                              {event.recipient}
                            </span>
                          ) : (
                            <span className="text-xs text-muted-foreground font-mono truncate max-w-[120px] block">
                              {event.email_id.substring(0, 8)}...
                            </span>
                          )}
                        </TableCell>
                        <TableCell className="hidden md:table-cell max-w-[250px]">
                          {(event.event_type === 'click' || event.event_type === 'clicked') && !!event.metadata?.url ? (
                            <span className="text-xs text-muted-foreground truncate block flex items-center gap-1">
                              <Globe className="h-3 w-3 shrink-0" />
                              {String(event.metadata!.url)}
                            </span>
                          ) : event.event_type === 'bounced' && !!event.metadata?.bounce_type ? (
                            <span className="text-xs text-muted-foreground">
                              {String(event.metadata!.bounce_type)}
                              {event.metadata!.bounce_sub_type != null && ` / ${String(event.metadata!.bounce_sub_type)}`}
                            </span>
                          ) : (
                            <span className="text-xs text-muted-foreground">--</span>
                          )}
                        </TableCell>
                        <TableCell className="hidden lg:table-cell">
                          <div className="flex items-center gap-2 text-xs text-muted-foreground">
                            {event.ip_address && (
                              <span className="flex items-center gap-1">
                                <Globe className="h-3 w-3" />
                                {event.ip_address}
                              </span>
                            )}
                            {event.user_agent && (
                              <span className="flex items-center gap-1">
                                <Monitor className="h-3 w-3" />
                                {parseUserAgent(event.user_agent)}
                              </span>
                            )}
                            {!event.ip_address && !event.user_agent && '--'}
                          </div>
                        </TableCell>
                        <TableCell className="text-muted-foreground text-sm whitespace-nowrap">
                          {formatDistanceToNow(new Date(event.created_at), {
                            addSuffix: true,
                          })}
                        </TableCell>
                      </TableRow>
                    ))}
                    {events?.events.length === 0 && (
                      <TableRow>
                        <TableCell colSpan={5} className="text-center text-muted-foreground py-8">
                          No events match the selected filter.
                        </TableCell>
                      </TableRow>
                    )}
                  </TableBody>
                </Table>
              </div>

              {/* Pagination */}
              {totalPages > 1 && (
                <div className="flex items-center justify-between pt-4">
                  <p className="text-sm text-muted-foreground hidden sm:inline">
                    Showing {(page - 1) * pageSize + 1} to{' '}
                    {Math.min(page * pageSize, events!.total)} of{' '}
                    {events!.total} events
                  </p>
                  <p className="text-sm text-muted-foreground sm:hidden">
                    {page} / {totalPages}
                  </p>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setPage((p) => p - 1)}
                      disabled={page === 1}
                    >
                      <ChevronLeft className="h-4 w-4" />
                      <span className="hidden sm:inline">Previous</span>
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => setPage((p) => p + 1)}
                      disabled={page >= totalPages}
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

'use client'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertTriangle,
  ChevronLeft,
  ChevronRight,
  Eye,
  Globe,
  Mail,
  MailWarning,
  Monitor,
  MousePointerClick,
  Send,
} from 'lucide-react'
import { useState } from 'react'

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

async function fetchEmailEvents(
  emailId: string,
  params: { event_type?: string; page?: number; page_size?: number }
): Promise<PaginatedEmailEvents> {
  const searchParams = new URLSearchParams()
  if (params.event_type) searchParams.set('event_type', params.event_type)

  const response = await fetch(`/api/emails/${emailId}/tracking/events?${searchParams}`)
  if (!response.ok) throw new Error('Failed to fetch email events')
  const events: EmailEvent[] = await response.json()

  // Backend returns a flat array; apply client-side pagination
  const page = params.page ?? 1
  const pageSize = params.page_size ?? 20
  const start = (page - 1) * pageSize
  const paged = events.slice(start, start + pageSize)

  return {
    events: paged,
    total: events.length,
    page,
    page_size: pageSize,
  }
}

function EventIcon({ type }: { type: string }) {
  switch (type) {
    case 'open':
    case 'opened':
      return <Eye className="h-4 w-4 text-blue-500" />
    case 'click':
    case 'clicked':
      return <MousePointerClick className="h-4 w-4 text-green-500" />
    case 'delivered':
      return <Send className="h-4 w-4 text-emerald-500" />
    case 'bounced':
      return <MailWarning className="h-4 w-4 text-red-500" />
    case 'complained':
      return <AlertTriangle className="h-4 w-4 text-orange-500" />
    default:
      return <Mail className="h-4 w-4 text-muted-foreground" />
  }
}

function EventBadge({ type }: { type: string }) {
  const variants: Record<string, { variant: 'default' | 'secondary' | 'destructive' | 'outline'; label: string }> = {
    open: { variant: 'secondary', label: 'Opened' },
    opened: { variant: 'secondary', label: 'Opened' },
    click: { variant: 'default', label: 'Clicked' },
    clicked: { variant: 'default', label: 'Clicked' },
    delivered: { variant: 'outline', label: 'Delivered' },
    bounced: { variant: 'destructive', label: 'Bounced' },
    complained: { variant: 'destructive', label: 'Complained' },
  }

  const config = variants[type] || { variant: 'outline' as const, label: type }

  return (
    <Badge variant={config.variant} className="gap-1 text-xs">
      <EventIcon type={type} />
      {config.label}
    </Badge>
  )
}

function parseUserAgent(ua: string): string {
  // Email image proxies (check first — they masquerade as browsers)
  if (ua.includes('GoogleImageProxy') || ua.includes('ggpht.com')) return 'Gmail (Google Proxy)'
  if (ua.includes('YahooMailProxy')) return 'Yahoo Mail (Proxy)'
  if (ua.includes('Outlook-iOS') || ua.includes('Outlook-Android')) return 'Outlook Mobile'
  // Email clients
  if (ua.includes('Gmail')) return 'Gmail'
  if (ua.includes('Yahoo')) return 'Yahoo Mail'
  if (ua.includes('Outlook') || ua.includes('Microsoft')) return 'Outlook'
  if (ua.includes('Thunderbird')) return 'Thunderbird'
  if (ua.includes('Apple Mail')) return 'Apple Mail'
  // Browsers
  if (ua.includes('Chrome') && !ua.includes('Chromium')) return 'Chrome'
  if (ua.includes('Firefox')) return 'Firefox'
  if (ua.includes('Safari') && !ua.includes('Chrome')) return 'Safari'
  if (ua.includes('AppleWebKit')) return 'WebKit'
  if (ua.length > 50) return ua.substring(0, 50) + '...'
  return ua
}

export function EmailEventTimeline({ emailId }: { emailId: string }) {
  const [eventType, setEventType] = useState<string | undefined>()
  const [page, setPage] = useState(1)
  const pageSize = 20

  const { data, isLoading, error } = useQuery({
    queryKey: ['email-events', emailId, eventType, page],
    queryFn: () =>
      fetchEmailEvents(emailId, {
        event_type: eventType,
        page,
        page_size: pageSize,
      }),
  })

  const totalPages = data ? Math.ceil(data.total / pageSize) : 0

  if (isLoading) {
    return (
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Activity</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {[1, 2, 3].map((i) => (
            <div key={i} className="flex items-start gap-3">
              <Skeleton className="h-8 w-8 rounded-full shrink-0" />
              <div className="space-y-1 flex-1">
                <Skeleton className="h-4 w-24" />
                <Skeleton className="h-3 w-48" />
              </div>
            </div>
          ))}
        </CardContent>
      </Card>
    )
  }

  if (error) {
    return (
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Activity</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">Failed to load events.</p>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <CardTitle className="text-base flex items-center gap-2">
            Activity
            {data && data.total > 0 && (
              <Badge variant="secondary" className="text-xs font-normal">
                {data.total}
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
            <SelectTrigger className="w-[140px] h-8 text-xs">
              <SelectValue placeholder="All events" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All events</SelectItem>
              <SelectItem value="delivered">Delivered</SelectItem>
              <SelectItem value="open">Opened</SelectItem>
              <SelectItem value="click">Clicked</SelectItem>
              <SelectItem value="bounced">Bounced</SelectItem>
              <SelectItem value="complained">Complained</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </CardHeader>
      <CardContent>
        {!data || data.events.length === 0 ? (
          <p className="text-sm text-muted-foreground text-center py-4">
            No tracking events recorded yet.
          </p>
        ) : (
          <div className="space-y-0">
            {data.events.map((event, index) => (
              <div
                key={event.id}
                className="flex items-start gap-3 py-3 relative"
              >
                {/* Timeline line */}
                {index < data.events.length - 1 && (
                  <div className="absolute left-[15px] top-[40px] bottom-0 w-px bg-border" />
                )}

                {/* Icon circle */}
                <div className="flex items-center justify-center h-8 w-8 rounded-full bg-muted shrink-0 z-10">
                  <EventIcon type={event.event_type} />
                </div>

                {/* Content */}
                <div className="flex-1 min-w-0 space-y-1">
                  <div className="flex items-center gap-2 flex-wrap">
                    <EventBadge type={event.event_type} />
                    <span className="text-xs text-muted-foreground">
                      {format(new Date(event.created_at), 'PPp')}
                    </span>
                  </div>

                  <div className="flex items-center gap-3 text-xs text-muted-foreground flex-wrap">
                    {(event.event_type === 'click' || event.event_type === 'clicked') && !!event.metadata?.url && (
                      <span className="flex items-center gap-1 truncate max-w-[300px]">
                        <Globe className="h-3 w-3 shrink-0" />
                        <span className="truncate">{String(event.metadata!.url)}</span>
                      </span>
                    )}

                    {event.recipient && (
                      <span className="flex items-center gap-1">
                        <Mail className="h-3 w-3 shrink-0" />
                        {event.recipient}
                      </span>
                    )}

                    {event.ip_address && (
                      <span className="flex items-center gap-1">
                        <Globe className="h-3 w-3 shrink-0" />
                        {event.ip_address}
                      </span>
                    )}

                    {event.user_agent && (
                      <span className="flex items-center gap-1">
                        <Monitor className="h-3 w-3 shrink-0" />
                        {parseUserAgent(event.user_agent)}
                      </span>
                    )}
                  </div>

                  {/* Bounce/complaint metadata */}
                  {(event.event_type === 'bounced' || event.event_type === 'complained') &&
                    event.metadata != null && (
                      <div className="text-xs bg-muted/50 rounded p-2 mt-1">
                        {!!event.metadata.bounce_type && (
                          <span>
                            Type: <strong>{String(event.metadata.bounce_type)}</strong>
                            {event.metadata.bounce_sub_type != null &&
                              ` (${String(event.metadata.bounce_sub_type)})`}
                          </span>
                        )}
                        {!!event.metadata.complaint_type && (
                          <span>
                            Type: <strong>{String(event.metadata.complaint_type)}</strong>
                          </span>
                        )}
                      </div>
                    )}
                </div>
              </div>
            ))}
          </div>
        )}

        {/* Pagination */}
        {totalPages > 1 && (
          <div className="flex items-center justify-between pt-4 border-t mt-2">
            <p className="text-xs text-muted-foreground">
              {data!.total} events
            </p>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() => setPage((p) => p - 1)}
                disabled={page === 1}
              >
                <ChevronLeft className="h-3 w-3" />
              </Button>
              <span className="text-xs">
                {page} / {totalPages}
              </span>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() => setPage((p) => p + 1)}
                disabled={page >= totalPages}
              >
                <ChevronRight className="h-3 w-3" />
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

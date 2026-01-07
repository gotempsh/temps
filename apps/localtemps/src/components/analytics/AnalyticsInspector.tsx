import { useState } from "react"
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query"
import { format, formatDistanceToNow } from "date-fns"
import {
  Loader2,
  Trash2,
  RefreshCw,
  Activity,
  Gauge,
  Monitor,
  MousePointer,
  Eye,
  Clock,
  Zap,
  ChevronDown,
  ChevronUp,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Badge } from "@/components/ui/badge"
import { ScrollArea } from "@/components/ui/scroll-area"
import { JsonViewer } from "@/components/ui/json-viewer"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { CopyButton } from "@/components/ui/copy-button"
import { cn } from "@/lib/utils"

const API_BASE = "http://localhost:4000"

interface AnalyticsEvent {
  id: number
  event_type: string
  event_name: string | null
  request_path: string | null
  request_query: string | null
  domain: string | null
  session_id: string | null
  request_id: string | null
  payload: unknown
  received_at: string
}

interface CountResponse {
  count: number
}

async function fetchEvents(limit = 50, offset = 0): Promise<AnalyticsEvent[]> {
  const res = await fetch(
    `${API_BASE}/api/inspector/events?limit=${limit}&offset=${offset}`
  )
  if (!res.ok) throw new Error("Failed to fetch events")
  return res.json()
}

async function fetchEventCount(): Promise<number> {
  const res = await fetch(`${API_BASE}/api/inspector/events/count`)
  if (!res.ok) throw new Error("Failed to fetch count")
  const data: CountResponse = await res.json()
  return data.count
}

async function clearAllEvents(): Promise<void> {
  const res = await fetch(`${API_BASE}/api/inspector/events`, {
    method: "DELETE",
  })
  if (!res.ok) throw new Error("Failed to clear events")
}

function getEventIcon(eventType: string, eventName: string | null) {
  if (eventType === "speed") return Gauge
  if (eventType === "session_init") return Monitor
  if (eventType === "session_events") return MousePointer

  // Regular events
  switch (eventName) {
    case "page_view":
      return Eye
    case "page_leave":
      return Clock
    case "heartbeat":
      return Activity
    default:
      return Zap
  }
}

function getEventBadgeVariant(eventType: string, eventName: string | null) {
  if (eventType === "speed") return "warning"
  if (eventType === "session_init" || eventType === "session_events") return "secondary"

  switch (eventName) {
    case "page_view":
      return "success"
    case "page_leave":
      return "outline"
    case "heartbeat":
      return "default"
    default:
      return "default"
  }
}

function EventCard({ event }: { event: AnalyticsEvent }) {
  const [isExpanded, setIsExpanded] = useState(false)
  const Icon = getEventIcon(event.event_type, event.event_name)
  const badgeVariant = getEventBadgeVariant(event.event_type, event.event_name) as "success" | "warning" | "secondary" | "outline" | "default" | "destructive"

  const displayName = event.event_name || event.event_type
  const receivedAt = new Date(event.received_at)

  return (
    <Collapsible open={isExpanded} onOpenChange={setIsExpanded}>
      <Card className={cn(
        "transition-all",
        isExpanded && "ring-1 ring-primary/20"
      )}>
        <CollapsibleTrigger asChild>
          <CardHeader className="py-3 px-4 cursor-pointer hover:bg-muted/50 transition-colors">
            <div className="flex items-center gap-3">
              <div className="p-1.5 rounded-md bg-muted">
                <Icon className="h-4 w-4 text-muted-foreground" />
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <Badge variant={badgeVariant} className="text-xs">
                    {displayName}
                  </Badge>
                  {event.request_path && (
                    <code className="text-xs text-muted-foreground truncate max-w-[200px]">
                      {event.request_path}
                    </code>
                  )}
                </div>
                <div className="flex items-center gap-2 text-xs text-muted-foreground mt-0.5">
                  <span title={format(receivedAt, "PPpp")}>
                    {formatDistanceToNow(receivedAt, { addSuffix: true })}
                  </span>
                  {event.session_id && (
                    <>
                      <span>•</span>
                      <span className="truncate max-w-[120px]" title={event.session_id}>
                        {event.session_id.slice(0, 8)}...
                      </span>
                    </>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-xs text-muted-foreground">#{event.id}</span>
                {isExpanded ? (
                  <ChevronUp className="h-4 w-4 text-muted-foreground" />
                ) : (
                  <ChevronDown className="h-4 w-4 text-muted-foreground" />
                )}
              </div>
            </div>
          </CardHeader>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <CardContent className="pt-0 pb-4 px-4">
            <div className="space-y-3">
              {/* Metadata */}
              <div className="grid grid-cols-2 gap-2 text-xs">
                {event.domain && (
                  <div>
                    <span className="text-muted-foreground">Domain:</span>{" "}
                    <code className="bg-muted px-1 rounded">{event.domain}</code>
                  </div>
                )}
                {event.request_query && (
                  <div>
                    <span className="text-muted-foreground">Query:</span>{" "}
                    <code className="bg-muted px-1 rounded">{event.request_query}</code>
                  </div>
                )}
                {event.session_id && (
                  <div className="col-span-2 flex items-center gap-1">
                    <span className="text-muted-foreground">Session:</span>{" "}
                    <code className="bg-muted px-1 rounded truncate flex-1">{event.session_id}</code>
                    <CopyButton value={event.session_id} size="sm" variant="ghost" className="h-5 w-5 p-0" />
                  </div>
                )}
                {event.request_id && (
                  <div className="col-span-2 flex items-center gap-1">
                    <span className="text-muted-foreground">Request ID:</span>{" "}
                    <code className="bg-muted px-1 rounded truncate flex-1">{event.request_id}</code>
                    <CopyButton value={event.request_id} size="sm" variant="ghost" className="h-5 w-5 p-0" />
                  </div>
                )}
                <div className="col-span-2">
                  <span className="text-muted-foreground">Received:</span>{" "}
                  <code className="bg-muted px-1 rounded">{format(receivedAt, "PPpp")}</code>
                </div>
              </div>

              {/* Payload */}
              <div>
                <div className="flex items-center justify-between mb-1">
                  <span className="text-xs font-medium text-muted-foreground">Payload</span>
                  <CopyButton
                    value={JSON.stringify(event.payload, null, 2)}
                    size="sm"
                    variant="ghost"
                    className="h-6 text-xs"
                  >
                    Copy JSON
                  </CopyButton>
                </div>
                <JsonViewer data={event.payload} />
              </div>
            </div>
          </CardContent>
        </CollapsibleContent>
      </Card>
    </Collapsible>
  )
}

function UsageInstructions() {
  const setupCode = `import { TempsAnalyticsProvider } from '@temps-sdk/react-analytics'

function App() {
  return (
    <TempsAnalyticsProvider
      basePath="http://localhost:4000/api/_temps"
      ignoreLocalhost={false}
    >
      <YourApp />
    </TempsAnalyticsProvider>
  )
}`

  const trackEventCode = `import { useTempsAnalytics } from '@temps-sdk/react-analytics'

function MyComponent() {
  const { trackEvent } = useTempsAnalytics()

  const handleClick = () => {
    trackEvent('button_clicked', { buttonId: 'save-btn' })
  }

  return <button onClick={handleClick}>Save</button>
}`

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base flex items-center gap-2">
          <Activity className="h-4 w-4" />
          How to Use
        </CardTitle>
        <CardDescription>
          Configure @temps-sdk/react-analytics to send events to LocalTemps
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <h4 className="text-sm font-medium">1. Setup Provider</h4>
            <CopyButton value={setupCode} size="sm" variant="ghost" className="h-7" />
          </div>
          <pre className="p-3 rounded-lg bg-muted font-mono text-xs overflow-x-auto">
            {setupCode}
          </pre>
        </div>
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <h4 className="text-sm font-medium">2. Track Custom Events</h4>
            <CopyButton value={trackEventCode} size="sm" variant="ghost" className="h-7" />
          </div>
          <pre className="p-3 rounded-lg bg-muted font-mono text-xs overflow-x-auto">
            {trackEventCode}
          </pre>
        </div>
      </CardContent>
    </Card>
  )
}

export function AnalyticsInspector() {
  const queryClient = useQueryClient()
  const [autoRefresh, setAutoRefresh] = useState(true)

  const {
    data: events = [],
    isLoading,
    isRefetching,
  } = useQuery({
    queryKey: ["analytics-events"],
    queryFn: () => fetchEvents(),
    refetchInterval: autoRefresh ? 3000 : false,
  })

  const { data: eventCount = 0 } = useQuery({
    queryKey: ["analytics-count"],
    queryFn: fetchEventCount,
    refetchInterval: autoRefresh ? 3000 : false,
  })

  const clearMutation = useMutation({
    mutationFn: clearAllEvents,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["analytics-events"] })
      queryClient.invalidateQueries({ queryKey: ["analytics-count"] })
    },
  })

  const handleRefresh = () => {
    queryClient.invalidateQueries({ queryKey: ["analytics-events"] })
    queryClient.invalidateQueries({ queryKey: ["analytics-count"] })
  }

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold">Analytics Inspector</h2>
          <Badge variant="secondary">{eventCount} events</Badge>
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setAutoRefresh(!autoRefresh)}
            className={cn(autoRefresh && "text-success")}
          >
            <RefreshCw className={cn("h-4 w-4", autoRefresh && "animate-spin-slow")} />
            {autoRefresh ? "Auto" : "Manual"}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={handleRefresh}
            disabled={isRefetching}
          >
            <RefreshCw className={cn("h-4 w-4", isRefetching && "animate-spin")} />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => clearMutation.mutate()}
            disabled={clearMutation.isPending || events.length === 0}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>

      {/* Events List */}
      {isLoading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </div>
      ) : events.length === 0 ? (
        <div className="space-y-4">
          <Card className="border-dashed">
            <CardContent className="flex flex-col items-center justify-center py-12 text-center">
              <Activity className="h-12 w-12 text-muted-foreground mb-4" />
              <h3 className="text-lg font-medium mb-1">No events captured yet</h3>
              <p className="text-sm text-muted-foreground max-w-sm">
                Configure your app with @temps-sdk/react-analytics to start capturing events.
              </p>
            </CardContent>
          </Card>
          <UsageInstructions />
        </div>
      ) : (
        <ScrollArea className="h-[calc(100vh-280px)]">
          <div className="space-y-2 pr-4">
            {events.map((event) => (
              <EventCard key={event.id} event={event} />
            ))}
          </div>
        </ScrollArea>
      )}
    </div>
  )
}

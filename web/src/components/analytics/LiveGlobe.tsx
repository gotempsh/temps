import { useMemo, useState, useCallback, useRef, useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getLiveVisitorsListOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  getRecentActivity,
  getEnvironments,
  type ActivityEvent,
  type EnvironmentResponse,
  type RecentActivityResponse,
} from '@/api/client'
import type { ProjectResponse, VisitorInfo } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Users,
  ArrowLeft,
  Globe as GlobeIcon,
  Pause,
  Play,
  FileText,
  Monitor,
  Smartphone,
  Tablet,
  Eye,
  Zap,
  ArrowUpRight,
} from 'lucide-react'
import { Link, useNavigate } from 'react-router'
import { EarthGlobe, type ProjectedMarker } from './EarthGlobe'

// ─── Types ───────────────────────────────────────────────────────

interface LiveGlobePageProps {
  project: ProjectResponse
}

// ─── Helpers ─────────────────────────────────────────────────────

function countryCodeToFlag(countryCode: string | null | undefined): string {
  if (!countryCode || countryCode.length !== 2) return ''
  const codePoints = countryCode
    .toUpperCase()
    .split('')
    .map((char) => 127397 + char.charCodeAt(0))
  return String.fromCodePoint(...codePoints)
}

function getTimeAgo(dateStr: string): string {
  const now = new Date()
  const date = new Date(dateStr)
  const diffMs = now.getTime() - date.getTime()
  const diffSecs = Math.floor(diffMs / 1000)
  if (diffSecs < 5) return 'just now'
  if (diffSecs < 60) return `${diffSecs}s ago`
  const diffMins = Math.floor(diffSecs / 60)
  if (diffMins < 60) return `${diffMins}m ago`
  const diffHours = Math.floor(diffMins / 60)
  return `${diffHours}h ago`
}

function getDeviceIcon(deviceType: string | null | undefined) {
  switch (deviceType?.toLowerCase()) {
    case 'mobile':
      return Smartphone
    case 'tablet':
      return Tablet
    default:
      return Monitor
  }
}

function getEventIcon(eventType: string) {
  switch (eventType) {
    case 'page_view':
      return Eye
    case 'custom':
      return Zap
    default:
      return FileText
  }
}

function getEventLabel(event: ActivityEvent): string {
  if (event.event_type === 'page_view') {
    return event.page_title || event.page_path
  }
  if (event.event_type === 'custom' && event.event_name) {
    return event.event_name
  }
  return event.event_type
}

function hashString(str: string): number {
  let hash = 0
  for (let i = 0; i < str.length; i++) {
    const char = str.charCodeAt(i)
    hash = (hash << 5) - hash + char
    hash |= 0
  }
  return Math.abs(hash)
}

const EVENT_COLORS = [
  'text-blue-400',
  'text-emerald-400',
  'text-violet-400',
  'text-amber-400',
  'text-cyan-400',
  'text-pink-400',
  'text-lime-400',
  'text-orange-400',
  'text-indigo-400',
  'text-teal-400',
]

function getVisitorColor(visitorId: number | null | undefined): string {
  if (visitorId == null) return 'text-muted-foreground'
  return EVENT_COLORS[hashString(String(visitorId)) % EVENT_COLORS.length]
}

// ─── Activity Feed Item ──────────────────────────────────────────

interface ActivityFeedItemProps {
  event: ActivityEvent
  isNew: boolean
  projectSlug: string
}

function ActivityFeedItem({ event, isNew, projectSlug }: ActivityFeedItemProps) {
  const flag = countryCodeToFlag(event.country_code)
  const location = [event.city, event.country].filter(Boolean).join(', ')
  const timeAgo = getTimeAgo(event.timestamp)
  const EventIcon = getEventIcon(event.event_type)
  const DeviceIcon = getDeviceIcon(event.device_type)
  const visitorColor = getVisitorColor(event.visitor_id)
  const label = getEventLabel(event)

  const visitorUrl = event.visitor_id
    ? `/projects/${projectSlug}/analytics/visitors/${event.visitor_id}`
    : undefined

  const content = (
    <div
      className={`flex items-start gap-3 py-2.5 px-3 rounded-lg transition-all duration-500 ${
        isNew
          ? 'bg-primary/5 border border-primary/20'
          : 'border border-transparent'
      } ${visitorUrl ? 'hover:bg-accent/50 cursor-pointer' : ''}`}
    >
      {/* Event type icon */}
      <div className={`mt-0.5 flex-shrink-0 ${visitorColor}`}>
        <EventIcon className="h-4 w-4" />
      </div>

      {/* Content */}
      <div className="min-w-0 flex-1 space-y-1">
        {/* Page path / event name */}
        <p className="text-sm font-medium truncate" title={label}>
          {event.event_type === 'page_view' ? (
            <span className="font-mono text-xs">{event.page_path}</span>
          ) : (
            label
          )}
        </p>

        {/* Meta line */}
        <div className="flex items-center gap-2 text-[11px] text-muted-foreground flex-wrap">
          {/* Location */}
          {location && (
            <span className="flex items-center gap-1">
              {flag && <span>{flag}</span>}
              {location}
            </span>
          )}

          {/* Device / browser */}
          {(event.browser || event.device_type) && (
            <span className="flex items-center gap-1">
              <DeviceIcon className="h-3 w-3" />
              {event.browser || event.device_type}
            </span>
          )}

          {/* Referrer */}
          {event.referrer && (
            <span className="flex items-center gap-1 truncate max-w-[120px]">
              <ArrowUpRight className="h-3 w-3 flex-shrink-0" />
              <span className="truncate">
                {event.referrer.replace(/^https?:\/\//, '').split('/')[0]}
              </span>
            </span>
          )}

          {/* Time */}
          <span className="ml-auto flex-shrink-0">{timeAgo}</span>
        </div>
      </div>
    </div>
  )

  if (visitorUrl) {
    return (
      <Link to={visitorUrl} className="no-underline text-inherit block">
        {content}
      </Link>
    )
  }

  return content
}

// ─── Main page component ─────────────────────────────────────────

export function LiveGlobePage({ project }: LiveGlobePageProps) {
  const navigate = useNavigate()
  const [projectedMarkers, setProjectedMarkers] = useState<ProjectedMarker[]>(
    []
  )
  const [isHovered, setIsHovered] = useState(false)
  const [isPaused, setIsPaused] = useState(false)
  const [selectedEnvironment, setSelectedEnvironment] = useState<
    number | undefined
  >(undefined)

  // Cursor-based polling state for activity feed
  const sinceIdRef = useRef<number | null>(null)
  const [activityEvents, setActivityEvents] = useState<ActivityEvent[]>([])
  const [newEventIds, setNewEventIds] = useState<Set<number>>(new Set())
  const maxEvents = 100

  // Fetch live visitors for globe markers
  const { data: liveData } = useQuery({
    ...getLiveVisitorsListOptions({
      query: {
        project_id: project.id,
        environment_id: selectedEnvironment,
        window_minutes: 5,
      },
    }),
    refetchInterval: isPaused ? false : 10000,
  })

  // Fetch recent activity with cursor-based polling
  const { data: activityData } = useQuery<RecentActivityResponse>({
    queryKey: [
      'getRecentActivity',
      {
        project_id: project.id,
        environment_id: selectedEnvironment,
        since_id: sinceIdRef.current,
      },
    ],
    queryFn: async ({ signal }) => {
      const response = await getRecentActivity({
        query: {
          project_id: project.id,
          limit: 50,
          ...(selectedEnvironment != null
            ? { environment_id: selectedEnvironment }
            : {}),
          ...(sinceIdRef.current != null
            ? { since_id: sinceIdRef.current }
            : {}),
        },
        signal,
        throwOnError: true,
      })
      return response.data
    },
    refetchInterval: isPaused ? false : 3000,
  })

  // Merge new activity events into the feed
  useEffect(() => {
    if (!activityData?.events?.length) return

    setActivityEvents((prev) => {
      const existingIds = new Set(prev.map((e) => e.id))
      const newEvents = activityData.events.filter(
        (e) => !existingIds.has(e.id)
      )

      if (newEvents.length === 0) return prev

      // Track new event IDs for highlight animation
      setNewEventIds(new Set(newEvents.map((e) => e.id)))

      // Update cursor to highest ID
      const maxId = Math.max(...activityData.events.map((e) => e.id))
      if (sinceIdRef.current === null || maxId > sinceIdRef.current) {
        sinceIdRef.current = maxId
      }

      // Merge and sort descending, cap at maxEvents
      const merged = [...newEvents, ...prev]
        .sort((a, b) => b.id - a.id)
        .slice(0, maxEvents)

      return merged
    })

    // Clear highlight after animation
    const timer = setTimeout(() => {
      setNewEventIds(new Set())
    }, 2000)

    return () => clearTimeout(timer)
  }, [activityData])

  // Build visitor list for globe from live data
  const allVisitors = useMemo(() => {
    const visitors: VisitorInfo[] = []
    if (liveData?.visitors) {
      for (const lv of liveData.visitors) {
        if (lv.latitude != null && lv.longitude != null) {
          visitors.push(lv as unknown as VisitorInfo)
        }
      }
    }
    return visitors
  }, [liveData])

  const liveVisitorIds = useMemo(
    () => new Set(liveData?.visitors?.map((v) => v.visitor_id) ?? []),
    [liveData]
  )

  const liveCount = liveData?.visitors?.length ?? 0

  const handleProjectedMarkersUpdate = useCallback(
    (markers: ProjectedMarker[]) => {
      setProjectedMarkers(markers)
    },
    []
  )

  // Environments query for the filter
  const { data: environments } = useQuery<EnvironmentResponse[]>({
    queryKey: ['getEnvironments', project.id],
    queryFn: async () => {
      const response = await getEnvironments({
        path: { project_id: project.id },
      })
      return response.data ?? []
    },
  })

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/projects/${project.slug}/analytics`)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="text-xl font-semibold flex items-center gap-2">
              <GlobeIcon className="h-5 w-5" />
              Live View
            </h2>
            <p className="text-sm text-muted-foreground">
              Real-time visitor activity on {project.name}
            </p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {/* Pause/Resume */}
          <Button
            variant="outline"
            size="sm"
            onClick={() => setIsPaused((p) => !p)}
            className="gap-1.5"
          >
            {isPaused ? (
              <>
                <Play className="h-3.5 w-3.5" />
                Resume
              </>
            ) : (
              <>
                <Pause className="h-3.5 w-3.5" />
                Pause
              </>
            )}
          </Button>

          {/* Environment filter */}
          {environments && environments.length > 0 && (
            <Select
              value={selectedEnvironment?.toString() ?? 'all'}
              onValueChange={(v) =>
                setSelectedEnvironment(v === 'all' ? undefined : parseInt(v))
              }
            >
              <SelectTrigger className="w-[160px] h-9">
                <SelectValue placeholder="All environments" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All environments</SelectItem>
                {environments.map((env) => (
                  <SelectItem key={env.id} value={env.id.toString()}>
                    {env.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}

          {/* Live badge */}
          {liveCount > 0 && (
            <Badge
              variant="outline"
              className="gap-1.5 border-green-500/50 text-green-500"
            >
              <span className="relative flex h-2 w-2">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-400 opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-green-500" />
              </span>
              {liveCount} live
            </Badge>
          )}

          <Badge variant="secondary" className="gap-1">
            <Users className="h-3 w-3" />
            {allVisitors.length} on globe
          </Badge>
        </div>
      </div>

      {/* Globe + Activity Feed layout */}
      <div className="flex flex-col lg:flex-row gap-4">
        {/* Globe container */}
        <div
          className="flex-1 rounded-lg border bg-card overflow-hidden relative min-h-[350px] sm:min-h-[550px]"
          onMouseEnter={() => setIsHovered(true)}
          onMouseLeave={() => setIsHovered(false)}
        >
          <EarthGlobe
            visitors={allVisitors}
            liveVisitorIds={liveVisitorIds}
            globeSize={550}
            paused={isHovered || isPaused}
            onProjectedMarkersUpdate={handleProjectedMarkersUpdate}
          />

          {/* Projected marker labels */}
          <GlobeMarkerOverlays
            projectedMarkers={projectedMarkers}
            liveVisitorIds={liveVisitorIds}
          />

          {/* Paused overlay */}
          {isPaused && (
            <div className="absolute top-3 left-3 bg-background/80 backdrop-blur-sm rounded-md px-2.5 py-1.5 text-xs text-muted-foreground border flex items-center gap-1.5">
              <Pause className="h-3 w-3" />
              Paused
            </div>
          )}
        </div>

        {/* Activity feed */}
        <div className="lg:w-[380px] rounded-lg border bg-card flex flex-col overflow-hidden">
          {/* Feed header */}
          <div className="px-4 py-3 border-b flex items-center justify-between flex-shrink-0">
            <p className="text-xs font-medium text-muted-foreground uppercase tracking-wider">
              Activity Feed
            </p>
            <span className="text-xs text-muted-foreground">
              {activityEvents.length} events
            </span>
          </div>

          {/* Feed content */}
          <div
            className="flex-1 overflow-y-auto p-2 space-y-0.5 max-h-[300px] sm:max-h-[502px]"
          >
            {activityEvents.length === 0 ? (
              <div className="flex flex-col items-center justify-center h-full text-center p-6">
                <Zap className="h-8 w-8 text-muted-foreground/40 mb-3" />
                <p className="text-sm text-muted-foreground">
                  Waiting for activity...
                </p>
                <p className="text-xs text-muted-foreground mt-1">
                  Events will appear here in real time
                </p>
              </div>
            ) : (
              activityEvents.map((event) => (
                <ActivityFeedItem
                  key={event.id}
                  event={event}
                  isNew={newEventIds.has(event.id)}
                  projectSlug={project.slug}
                />
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

// ─── Simplified globe marker overlays (no popover, minimal labels) ─

interface GlobeMarkerOverlaysProps {
  projectedMarkers: ProjectedMarker[]
  liveVisitorIds: Set<string>
}

function GlobeMarkerOverlays({
  projectedMarkers,
  liveVisitorIds,
}: GlobeMarkerOverlaysProps) {
  return (
    <div className="absolute inset-0 pointer-events-none overflow-hidden">
      {projectedMarkers.map((pm) => {
        const opacity = Math.min(1, pm.z * 2)
        const isLive = liveVisitorIds.has(pm.visitor.visitor_id)
        const flag = countryCodeToFlag(pm.visitor.country_code)
        const city = pm.visitor.city || pm.visitor.country || ''

        return (
          <div
            key={pm.visitor.id}
            className="absolute transition-opacity duration-150"
            style={{
              left: pm.x,
              top: pm.y,
              opacity,
              zIndex: Math.round(pm.z * 100),
              transform: 'translate(-50%, -50%)',
            }}
          >
            <div className="flex items-center gap-1 rounded-full px-2 py-0.5 bg-background/70 backdrop-blur-sm border border-border/40 text-[10px]">
              {flag && <span>{flag}</span>}
              <span className="truncate max-w-[80px]">{city}</span>
              {isLive && (
                <span className="relative flex h-1.5 w-1.5 flex-shrink-0">
                  <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-400 opacity-75" />
                  <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-green-500" />
                </span>
              )}
            </div>
          </div>
        )
      })}
    </div>
  )
}

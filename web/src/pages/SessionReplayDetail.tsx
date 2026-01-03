import { ProjectResponse, SessionEventDto } from '@/api/client'
import {
  getSessionReplayEventsOptions,
  getSessionReplayOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { SessionReplayPlayer } from '@/components/session-replay/SessionReplayPlayer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  Brush,
  Camera,
  ChevronRight,
  Eye,
  FileCode,
  FileEdit,
  FileText,
  Keyboard,
  Loader2,
  Maximize2,
  Mouse,
  MousePointer,
  MousePointerClick,
  Move,
  PaintBucket,
  Palette,
  Play,
  Puzzle,
  ScrollText,
  Smartphone,
  Sparkles,
  Terminal,
  TextSelect,
  Type,
  User,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'

// Type definitions for event data
interface IncrementalSnapshotData {
  source?: number
  [key: string]: unknown
}

interface MetaEventData {
  href?: string
  [key: string]: unknown
}

// Type guards
function isIncrementalSnapshotData(
  data: unknown
): data is IncrementalSnapshotData {
  return (
    typeof data === 'object' &&
    data !== null &&
    ('source' in data
      ? typeof (data as IncrementalSnapshotData).source === 'number'
      : true)
  )
}

function isMetaEventData(data: unknown): data is MetaEventData {
  return (
    typeof data === 'object' &&
    data !== null &&
    ('href' in data ? typeof (data as MetaEventData).href === 'string' : true)
  )
}

// Event type mapping
const EVENT_TYPE_INFO = {
  0: { name: 'DOMContentLoaded', icon: FileText, color: 'text-blue-500' },
  1: { name: 'Load', icon: Loader2, color: 'text-green-500' },
  2: { name: 'Full Snapshot', icon: Camera, color: 'text-purple-500' },
  3: { name: 'Incremental', icon: MousePointer, color: 'text-yellow-500' },
  4: { name: 'Meta', icon: Eye, color: 'text-cyan-500' },
  5: { name: 'Custom', icon: Sparkles, color: 'text-pink-500' },
  6: { name: 'Plugin', icon: Puzzle, color: 'text-indigo-500' },
}

// Incremental snapshot types
const INCREMENTAL_TYPES = {
  0: 'Mutation',
  1: 'Mouse Move',
  2: 'Mouse Interaction',
  3: 'Scroll',
  4: 'Viewport Resize',
  5: 'Input',
  6: 'Touch Move',
  7: 'Media Interaction',
  8: 'Style Sheet Rule',
  9: 'Canvas Mutation',
  10: 'Font',
  11: 'Log',
  12: 'Drag',
  13: 'Style Declaration',
  14: 'Selection',
  15: 'Adopted Style Sheet',
}

const getEventDescription = (event: SessionEventDto): string => {
  const eventInfo =
    EVENT_TYPE_INFO[event.event_type as keyof typeof EVENT_TYPE_INFO]

  // For incremental snapshots, get more detail
  if (event.event_type === 3 && isIncrementalSnapshotData(event.data)) {
    if (event.data.source !== undefined) {
      const incrementalType =
        INCREMENTAL_TYPES[
          event.data.source as keyof typeof INCREMENTAL_TYPES
        ] || 'Unknown'
      return incrementalType
    }
  }

  // For meta events, show URL if available
  if (event.event_type === 4 && isMetaEventData(event.data)) {
    if (event.data.href) {
      try {
        const url = new URL(event.data.href)
        return url.pathname
      } catch {
        return event.data.href
      }
    }
  }

  return eventInfo?.name || `Event ${event.event_type}`
}

const getEventIcon = (event: SessionEventDto) => {
  const eventInfo =
    EVENT_TYPE_INFO[event.event_type as keyof typeof EVENT_TYPE_INFO]

  // Special icons for incremental snapshot types
  if (event.event_type === 3 && isIncrementalSnapshotData(event.data)) {
    if (event.data.source !== undefined) {
      const iconMap: Record<
        number,
        React.ComponentType<{ className?: string }>
      > = {
        0: FileEdit, // Mutation
        1: Mouse, // Mouse Move
        2: MousePointerClick, // Mouse Interaction
        3: ScrollText, // Scroll
        4: Maximize2, // Viewport Resize
        5: Keyboard, // Input
        6: Smartphone, // Touch Move
        7: Play, // Media Interaction
        8: Palette, // Style Sheet Rule
        9: Brush, // Canvas Mutation
        10: Type, // Font
        11: Terminal, // Log
        12: Move, // Drag
        13: PaintBucket, // Style Declaration
        14: TextSelect, // Selection
        15: FileCode, // Adopted Style Sheet
      }
      return iconMap[event.data.source] || MousePointer
    }
  }

  return eventInfo?.icon || MousePointer
}

const getEventColor = (event: SessionEventDto) => {
  const eventInfo =
    EVENT_TYPE_INFO[event.event_type as keyof typeof EVENT_TYPE_INFO]
  return eventInfo?.color || 'text-gray-500'
}

export function SessionReplayDetail({ project }: { project: ProjectResponse }) {
  const { visitorId, sessionId } = useParams<{
    visitorId: string
    sessionId: string
    slug: string
  }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [selectedEvent, setSelectedEvent] = useState<SessionEventDto | null>(
    null
  )

  usePageTitle(`Session Replay - ${sessionId}`)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Visitors', href: '/visitors' },
      { label: visitorId || '', href: `/visitors/${visitorId}` },
      { label: 'Session Replay' },
      { label: sessionId?.slice(0, 8) || '' },
    ])
  }, [setBreadcrumbs, visitorId, sessionId])

  const {
    data: sessionData,
    isLoading,
    error,
  } = useQuery({
    ...getSessionReplayOptions({
      path: {
        visitor_id: Number(visitorId) || 0,
        session_id: Number(sessionId) || 0,
      },
    }),
    enabled: !!visitorId && !!sessionId,
  })
  const { data: eventsData } = useQuery({
    ...getSessionReplayEventsOptions({
      path: {
        session_id: Number(sessionId) || 0,
        visitor_id: Number(visitorId) || 0,
      },
    }),
  })
  const events = useMemo(() => eventsData?.events || [], [eventsData])

  // Group consecutive events of the same type
  const groupedEvents = useMemo(() => {
    if (!events.length) return []

    const groups: Array<{
      events: SessionEventDto[]
      type: number
      subType?: number
      startTime: number
      endTime: number
      count: number
    }> = []

    let currentGroup: (typeof groups)[0] | null = null

    events.forEach((event) => {
      const subType =
        event.event_type === 3 && isIncrementalSnapshotData(event.data)
          ? event.data.source
          : undefined

      // Check if should group with previous
      const shouldGroup =
        currentGroup &&
        currentGroup.type === event.event_type &&
        currentGroup.subType === subType &&
        event.event_type === 3 // Only group incremental events

      if (shouldGroup && currentGroup) {
        currentGroup.events.push(event)
        currentGroup.endTime = event.timestamp
        currentGroup.count++
      } else {
        if (currentGroup) groups.push(currentGroup)
        currentGroup = {
          events: [event],
          type: event.event_type || 0,
          subType,
          startTime: event.timestamp,
          endTime: event.timestamp,
          count: 1,
        }
      }
    })

    if (currentGroup) groups.push(currentGroup)
    return groups
  }, [events])

  const firstTimestamp = events[0]?.timestamp
  const getRelativeTime = (timestamp: number) => {
    if (!firstTimestamp) return '00:00'
    const ms = timestamp - firstTimestamp
    const seconds = Math.floor(ms / 1000)
    const minutes = Math.floor(seconds / 60)
    const secs = seconds % 60
    return `${minutes.toString().padStart(2, '0')}:${secs.toString().padStart(2, '0')}`
  }

  // Loading skeleton
  if (isLoading) {
    return (
      <div className="container max-w-full py-6">
        {/* Header skeleton */}
        <div className="mb-4 flex items-center justify-between">
          <Skeleton className="h-9 w-32" />
          <div className="flex items-center gap-3">
            <Skeleton className="h-12 w-28" />
            <Skeleton className="h-12 w-24" />
            <Skeleton className="h-12 w-32" />
          </div>
        </div>

        <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
          {/* Player skeleton */}
          <div className="lg:col-span-2">
            <Card>
              <CardContent className="p-0">
                <Skeleton className="h-[500px] w-full rounded-t-lg" />
                <div className="p-4 space-y-3">
                  <div className="flex items-center gap-2">
                    <Skeleton className="h-10 w-10 rounded-full" />
                    <Skeleton className="h-10 flex-1" />
                  </div>
                  <Skeleton className="h-2 w-full" />
                </div>
              </CardContent>
            </Card>
          </div>

          {/* Events timeline skeleton */}
          <div className="lg:col-span-1 h-[calc(100vh-200px)]">
            <Card className="h-full">
              <CardHeader className="pb-3">
                <div className="flex items-center justify-between">
                  <Skeleton className="h-5 w-16" />
                  <Skeleton className="h-5 w-32" />
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                {/* User info skeleton */}
                <div className="flex items-center gap-2 pb-3 border-b">
                  <Skeleton className="h-8 w-8 rounded-full" />
                  <div className="flex-1 space-y-1">
                    <Skeleton className="h-4 w-24" />
                    <Skeleton className="h-3 w-16" />
                  </div>
                  <Skeleton className="h-4 w-4" />
                </div>
                {/* Event items skeleton */}
                {Array.from({ length: 8 }).map((_, i) => (
                  <div key={i} className="flex items-start gap-3">
                    <Skeleton className="h-4 w-12" />
                    <Skeleton className="h-4 w-4" />
                    <div className="flex-1 space-y-1">
                      <Skeleton className="h-4 w-32" />
                      <Skeleton className="h-3 w-full" />
                    </div>
                  </div>
                ))}
              </CardContent>
            </Card>
          </div>
        </div>
      </div>
    )
  }

  // Calculate session stats
  const duration = sessionData?.session?.duration || 0
  const formatDuration = (ms: number) => {
    const seconds = Math.floor(ms / 1000)
    const minutes = Math.floor(seconds / 60)
    const hours = Math.floor(minutes / 60)

    if (hours > 0) {
      return `${hours}h ${minutes % 60}m`
    } else if (minutes > 0) {
      return `${minutes}m ${seconds % 60}s`
    } else {
      return `${seconds}s`
    }
  }

  // Helper function to format event data preview
  const formatEventDataPreview = (eventData: any): string => {
    // Check for meta event with href
    if (isMetaEventData(eventData)) {
      if (eventData.href) return eventData.href
    }

    // Check for incremental snapshot with source
    if (isIncrementalSnapshotData(eventData)) {
      if (eventData.source !== undefined) {
        return INCREMENTAL_TYPES[
          eventData.source as keyof typeof INCREMENTAL_TYPES
        ]
      }
    }

    // Fallback to JSON stringify
    const str = JSON.stringify(eventData)
    return str.length > 50 ? str.slice(0, 50) + '...' : str
  }

  // Helper function to format full event data
  const formatEventData = (data: unknown): string => {
    return typeof data === 'object' && data !== null
      ? JSON.stringify(data, null, 2)
      : String(data)
  }

  return (
    <div className="container max-w-full py-6">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="sm" asChild>
            <Link
              to={`/projects/${project.slug}/analytics/visitors/${visitorId}`}
            >
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back to Visitor
            </Link>
          </Button>
        </div>

        {/* Session Stats - Aligned to the right */}
        <div className="flex items-center gap-3">
          <Card className="border-0 shadow-none bg-muted/30">
            <CardContent className="flex items-center gap-2 p-3">
              <div className="text-xs text-muted-foreground">Duration</div>
              <div className="font-semibold">{formatDuration(duration)}</div>
            </CardContent>
          </Card>

          <Card className="border-0 shadow-none bg-muted/30">
            <CardContent className="flex items-center gap-2 p-3">
              <div className="text-xs text-muted-foreground">Events</div>
              <div className="font-semibold">{events.length}</div>
            </CardContent>
          </Card>

          <Card className="border-0 shadow-none bg-muted/30">
            <CardContent className="flex items-center gap-2 p-3">
              <div className="text-xs text-muted-foreground">Viewport</div>
              <div className="font-semibold">
                {sessionData?.session?.viewport_width || 0}×
                {sessionData?.session?.viewport_height || 0}
              </div>
            </CardContent>
          </Card>
        </div>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {/* Left side - Session Replay Player */}
        <div className="lg:col-span-2">
          <SessionReplayPlayer
            events={events}
            sessionData={{
              id: sessionId || '',
              created_at: sessionData?.session?.created_at || '',
              url: sessionData?.session?.url || '',
              duration: sessionData?.session?.duration || 0,
              event_count: events.length,
              viewport_width: sessionData?.session?.viewport_width || 0,
              viewport_height: sessionData?.session?.viewport_height || 0,
            }}
            isLoading={isLoading}
            error={error ? 'Failed to load session replay events' : null}
          />
        </div>

        {/* Right side - Events Timeline */}
        <div className="lg:col-span-1 h-[calc(100vh-200px)]">
          <Card className="h-full flex flex-col">
            <CardHeader className="pb-3 flex-shrink-0">
              <div className="flex items-center justify-between">
                <CardTitle className="text-base">Events</CardTitle>
                <Badge variant="secondary" className="text-xs">
                  {events.length} captured ({groupedEvents.length} groups)
                </Badge>
              </div>
            </CardHeader>
            <CardContent className="flex-1 p-0 overflow-hidden">
              <div className="flex flex-col h-full">
                {/* User info */}
                {visitorId && (
                  <div className="px-4 pb-3 border-b flex-shrink-0">
                    <Link
                      to={`/projects/${project.slug}/analytics/visitors/${visitorId}`}
                      className="flex items-center gap-2 hover:bg-muted/50 rounded-md p-2 -m-2 transition-colors"
                    >
                      <div className="h-8 w-8 rounded-full bg-primary/10 flex items-center justify-center">
                        <User className="h-4 w-4 text-primary" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <div className="text-sm font-medium truncate">
                          {visitorId.slice(0, 12)}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          View User
                        </div>
                      </div>
                      <ChevronRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />
                    </Link>
                  </div>
                )}

                {/* Events list with fixed height */}
                <ScrollArea className="flex-1 h-full">
                  <div className="divide-y">
                    {groupedEvents.map((group, index) => {
                      const firstEvent = group.events[0]
                      const firstEventData = firstEvent.data as any
                      const Icon = getEventIcon(firstEvent)
                      const color = getEventColor(firstEvent)
                      const description = getEventDescription(firstEvent)
                      const isSelected = selectedEvent?.id === firstEvent.id

                      return (
                        <div
                          key={`${group.startTime}-${index}`}
                          className={`px-4 py-3 hover:bg-muted/50 cursor-pointer transition-colors ${
                            isSelected ? 'bg-muted' : ''
                          }`}
                          onClick={() => setSelectedEvent(firstEvent)}
                        >
                          <div className="flex items-start gap-3">
                            <div className="text-xs text-muted-foreground mt-0.5 w-12">
                              {getRelativeTime(group.startTime)}
                            </div>
                            <Icon
                              className={`h-4 w-4 mt-0.5 flex-shrink-0 ${color}`}
                            />
                            <div className="flex-1 min-w-0">
                              <div className="text-sm font-medium">
                                {description}
                                {group.count > 1 && (
                                  <span className="text-muted-foreground font-normal ml-1">
                                    (×{group.count})
                                  </span>
                                )}
                              </div>
                              {firstEventData && (
                                <div className="text-xs text-muted-foreground mt-1 font-mono truncate">
                                  {formatEventDataPreview(firstEventData)}
                                </div>
                              )}
                            </div>
                          </div>

                          {isSelected && firstEventData && (
                            <div className="mt-3 ml-[60px] p-2 bg-muted/30 rounded-md">
                              <pre className="text-xs overflow-x-auto">
                                {formatEventData(firstEventData)}
                              </pre>
                            </div>
                          )}
                        </div>
                      )
                    })}
                  </div>
                </ScrollArea>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  )
}

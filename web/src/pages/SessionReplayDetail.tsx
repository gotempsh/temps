// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'

import { ProjectResponse, SessionEventDto } from '@/api/client'
import {
  getSessionReplayEventsOptions,
  getSessionReplayOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { SessionReplayPlayer } from '@/components/session-replay/SessionReplayPlayer'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  Button,
  Detail,
  fmtDateTime,
  fmtDuration,
  fmtRelativeTime,
  useUrlState,
  type DetailFact,
} from '@temps-sdk/ds'
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
import { useEffect, useMemo } from 'react'
import { Link, useParams } from 'react-router'

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
  const { get, patch } = useUrlState<'event'>()
  const selectedEventId = get('event')

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
      <Detail
        title={<Skeleton className="h-7 w-48" />}
        actions={<Skeleton className="h-9 w-32" />}
        facts={[0, 1, 2, 3].map(() => ({
          label: <Skeleton className="h-3 w-16" />,
          value: <Skeleton className="h-4 w-20" />,
        }))}
        main={
          <Card>
            <CardContent className="p-0">
              <Skeleton className="h-[500px] w-full rounded-t-lg" />
              <div className="space-y-3 p-4">
                <div className="flex items-center gap-2">
                  <Skeleton className="h-10 w-10 rounded-full" />
                  <Skeleton className="h-10 flex-1" />
                </div>
                <Skeleton className="h-2 w-full" />
              </div>
            </CardContent>
          </Card>
        }
        aside={
          <Card className="h-[calc(100vh-220px)]">
            <CardHeader className="pb-3">
              <div className="flex items-center justify-between">
                <Skeleton className="h-5 w-16" />
                <Skeleton className="h-5 w-32" />
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              {/* User info skeleton */}
              <div className="flex items-center gap-2 border-b pb-3">
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
        }
      />
    )
  }

  // Calculate session stats
  const duration = sessionData?.session?.duration || 0

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

  const facts: DetailFact[] = [
    { label: 'Duration', value: fmtDuration(duration) },
    { label: 'Events', value: events.length },
    {
      label: 'Viewport',
      value: `${sessionData?.session?.viewport_width || 0}×${sessionData?.session?.viewport_height || 0}`,
    },
    {
      label: 'Started',
      value: sessionData?.session?.created_at ? (
        <span title={fmtDateTime(sessionData.session.created_at)}>
          {fmtRelativeTime(sessionData.session.created_at)}
        </span>
      ) : (
        '—'
      ),
    },
  ]

  return (
    <Detail
      title={sessionId ? `Session ${sessionId.slice(0, 8)}` : 'Session'}
      actions={
        <Button variant="ghost" size="sm" asChild>
          <Link
            to={`/projects/${project.slug}/analytics/visitors/${visitorId}`}
          >
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Visitor
          </Link>
        </Button>
      }
      facts={facts}
      main={
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
      }
      aside={
        <Card className="flex h-[calc(100vh-220px)] flex-col">
          <CardHeader className="flex-shrink-0 pb-3">
            <div className="flex items-center justify-between">
              <CardTitle className="text-base">Events</CardTitle>
              <Badge variant="secondary" className="text-xs">
                {events.length} captured ({groupedEvents.length} groups)
              </Badge>
            </div>
          </CardHeader>
          <CardContent className="flex-1 overflow-hidden p-0">
            <div className="flex h-full flex-col">
              {/* User info */}
              {visitorId && (
                <div className="flex-shrink-0 border-b px-4 pb-3">
                  <Link
                    to={`/projects/${project.slug}/analytics/visitors/${visitorId}`}
                    className="-m-2 flex items-center gap-2 rounded-md p-2 transition-colors hover:bg-muted/50"
                  >
                    <div className="flex h-8 w-8 items-center justify-center rounded-full bg-primary/10">
                      <User className="h-4 w-4 text-primary" />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">
                        {visitorId.slice(0, 12)}
                      </div>
                      <div className="text-xs text-muted-foreground">
                        View User
                      </div>
                    </div>
                    <ChevronRight className="h-4 w-4 flex-shrink-0 text-muted-foreground" />
                  </Link>
                </div>
              )}

              {/* Events list with fixed height */}
              <ScrollArea className="h-full flex-1">
                <div className="divide-y">
                  {groupedEvents.map((group, index) => {
                    const firstEvent = group.events[0]
                    const firstEventData = firstEvent.data as any
                    const Icon = getEventIcon(firstEvent)
                    const color = getEventColor(firstEvent)
                    const description = getEventDescription(firstEvent)
                    const isSelected = selectedEventId === String(firstEvent.id)

                    return (
                      <div
                        key={`${group.startTime}-${index}`}
                        className={`cursor-pointer px-4 py-3 transition-colors hover:bg-muted/50 ${
                          isSelected ? 'bg-muted' : ''
                        }`}
                        onClick={() =>
                          patch({
                            event: isSelected ? undefined : firstEvent.id,
                          })
                        }
                      >
                        <div className="flex items-start gap-3">
                          <div className="mt-0.5 w-12 text-xs text-muted-foreground">
                            {getRelativeTime(group.startTime)}
                          </div>
                          <Icon
                            className={`mt-0.5 h-4 w-4 flex-shrink-0 ${color}`}
                          />
                          <div className="min-w-0 flex-1">
                            <div className="text-sm font-medium">
                              {description}
                              {group.count > 1 && (
                                <span className="ml-1 font-normal text-muted-foreground">
                                  (×{group.count})
                                </span>
                              )}
                            </div>
                            {firstEventData && (
                              <div className="mt-1 truncate font-mono text-xs text-muted-foreground">
                                {formatEventDataPreview(firstEventData)}
                              </div>
                            )}
                          </div>
                        </div>

                        {isSelected && firstEventData && (
                          <div className="ml-[60px] mt-3 rounded-md bg-muted/30 p-2">
                            <pre className="overflow-x-auto text-xs">
                              <HighlightedCode
                                code={formatEventData(firstEventData)}
                                language="json"
                              />
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
      }
    />
  )
}

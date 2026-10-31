// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
import rrwebPlayer from 'rrweb-player'
import 'rrweb-player/dist/style.css'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Loader2, AlertCircle, Monitor, Globe } from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { format } from 'date-fns'
import { SessionEventDto } from '@/api/client'

// rrweb-player 2.x types `props.events` as `eventWithTime[]` (from @rrweb/types,
// a transitive dependency), while the generated SDK ships event data as
// `unknown`. Derive the event type from the player's own constructor so we
// don't import from a package we don't directly depend on.
type RRwebEvent = ConstructorParameters<
  typeof rrwebPlayer
>[0]['props']['events'][number]

// rrweb bundles the Svelte runtime, but its published lifecycle types depend
// on a Svelte development dependency that React consumers do not install.
type ManagedReplayPlayer = rrwebPlayer & {
  $set(props: { width: number; height: number }): void
  $destroy(): void
}

interface SessionReplayPlayerProps {
  events: SessionEventDto[]
  sessionData?: {
    id: string
    created_at?: string
    url?: string
    duration?: number
    event_count?: number
    user_agent?: string
    viewport_width?: number
    viewport_height?: number
    page_count?: number
  }
  isLoading?: boolean
  error?: string | null
}

export function SessionReplayPlayer({
  events,
  sessionData,
  isLoading = false,
  error = null,
}: SessionReplayPlayerProps) {
  const playerContainerRef = useRef<HTMLDivElement>(null)
  const recordedWidth = sessionData?.viewport_width || 1280
  const recordedHeight = sessionData?.viewport_height || 720

  useEffect(() => {
    const container = playerContainerRef.current
    if (!container || !events.length || isLoading || error) return

    let player: ManagedReplayPlayer | undefined
    let animationFrame: number | undefined
    let previousWidth = 0
    let previousHeight = 0

    const fitPlayer = () => {
      // rrweb handles its own fullscreen dimensions and restores them on exit.
      if (
        document.fullscreenElement &&
        container.contains(document.fullscreenElement)
      ) {
        return
      }
      const availableWidth = container.clientWidth
      if (!availableWidth) return
      const scale = Math.min(
        availableWidth / recordedWidth,
        Math.max(1, window.innerHeight - 300) / recordedHeight,
        1
      )
      const width = Math.max(1, Math.floor(recordedWidth * scale))
      const height = Math.max(1, Math.floor(recordedHeight * scale))
      if (width === previousWidth && height === previousHeight) return
      previousWidth = width
      previousHeight = height

      if (player) {
        // Resize in place so changing the layout keeps the playback position.
        player.$set({ width, height })
        player.triggerResize()
        return
      }
      try {
        player = new rrwebPlayer({
          target: container,
          props: {
            events: events.map((event) => event.data as RRwebEvent),
            width,
            height,
            autoPlay: false,
            showController: true,
            skipInactive: true,
            speed: 1, // Default speed
            speedOption: [1, 2, 4, 8], // Available speed options
            mouseTail: {
              strokeStyle: '#667eea',
            },
            UNSAFE_replayCanvas: false,
          },
        }) as ManagedReplayPlayer
      } catch (error) {
        console.error('Failed to initialize replay player:', error)
      }
    }

    const scheduleFit = () => {
      if (animationFrame !== undefined) cancelAnimationFrame(animationFrame)
      animationFrame = requestAnimationFrame(fitPlayer)
    }
    const observer = new ResizeObserver(scheduleFit)
    observer.observe(container)
    window.addEventListener('resize', scheduleFit)
    document.addEventListener('fullscreenchange', scheduleFit)
    fitPlayer()

    return () => {
      observer.disconnect()
      window.removeEventListener('resize', scheduleFit)
      document.removeEventListener('fullscreenchange', scheduleFit)
      if (animationFrame !== undefined) cancelAnimationFrame(animationFrame)
      player?.$destroy()
    }
  }, [events, recordedWidth, recordedHeight, isLoading, error])

  if (isLoading) {
    return (
      <Card>
        <CardContent className="flex items-center justify-center min-h-[400px]">
          <div className="text-center space-y-4">
            <Loader2 className="h-12 w-12 animate-spin mx-auto text-muted-foreground" />
            <p className="text-muted-foreground">Loading session replay...</p>
          </div>
        </CardContent>
      </Card>
    )
  }

  if (error) {
    return (
      <Card>
        <CardContent className="p-6">
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  if (!events || events.length === 0) {
    return (
      <Card>
        <CardContent className="flex items-center justify-center min-h-[400px]">
          <div className="text-center space-y-4">
            <Monitor className="h-16 w-16 mx-auto text-muted-foreground" />
            <div>
              <p className="text-lg font-medium">No Replay Data</p>
              <p className="text-sm text-muted-foreground">
                No events recorded for this session
              </p>
            </div>
          </div>
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="min-w-0 space-y-4">
      {/* Player */}
      <Card className="overflow-hidden">
        <CardHeader>
          <CardTitle className="flex items-center justify-between">
            <span className="flex items-center gap-2">
              <Monitor className="h-5 w-5" />
              Session replay
            </span>
            {sessionData?.created_at && (
              <Badge variant="outline" className="font-normal">
                {format(
                  new Date(sessionData.created_at),
                  'MMM d, yyyy HH:mm:ss'
                )}
              </Badge>
            )}
          </CardTitle>
          {sessionData?.url && (
            <CardDescription className="flex items-center gap-2">
              <Globe className="h-3 w-3" />
              {sessionData.url}
            </CardDescription>
          )}
        </CardHeader>
        <CardContent className="p-0">
          <div className="bg-muted/50 p-4">
            <div
              ref={playerContainerRef}
              className="flex w-full min-w-0 justify-center"
            />
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

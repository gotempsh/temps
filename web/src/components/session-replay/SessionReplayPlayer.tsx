import { useCallback, useEffect, useRef } from 'react'
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
  const playerRef = useRef<any>(null)
  const initPlayer = useCallback(
    (eventData: SessionEventDto[]) => {
      if (!playerContainerRef.current) return

      // Destroy existing player if any
      if (playerRef.current) {
        playerRef.current.destroy?.()
      }

      try {
        // Clear container
        playerContainerRef.current.innerHTML = ''

        // Get the viewport dimensions from the session data or use defaults
        const recordedWidth = sessionData?.viewport_width || 1280
        const recordedHeight = sessionData?.viewport_height || 720

        // Calculate scale to fit within viewport
        const containerWidth = window.innerWidth - 80 // Account for padding
        const containerHeight = window.innerHeight - 300 // Account for header and controls

        const scaleX = containerWidth / recordedWidth
        const scaleY = containerHeight / recordedHeight
        const scale = Math.min(scaleX, scaleY, 1) // Don't scale up beyond 100%

        const width = Math.floor(recordedWidth * scale)
        const height = Math.floor(recordedHeight * scale)

        // Create new player instance
        playerRef.current = new rrwebPlayer({
          target: playerContainerRef.current,
          props: {
            events: eventData.map((event) => event.data),
            width: width,
            height: height,
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
        })
      } catch (error) {
        console.error('Failed to initialize replay player:', error)
      }
    },
    [sessionData]
  )

  useEffect(() => {
    // Initialize player when events are available
    if (events && events.length > 0 && playerContainerRef.current) {
      initPlayer(events)
    }

    // Cleanup on unmount
    return () => {
      if (playerRef.current) {
        playerRef.current.destroy?.()
      }
    }
  }, [events, initPlayer])

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
    <div className="space-y-4">
      {/* Player */}
      <Card className="overflow-hidden">
        <CardHeader>
          <CardTitle className="flex items-center justify-between">
            <span className="flex items-center gap-2">
              <Monitor className="h-5 w-5" />
              Session Replay
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
          <div className="bg-muted/50 p-4 flex justify-center">
            <div ref={playerContainerRef} className="mx-auto" />
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

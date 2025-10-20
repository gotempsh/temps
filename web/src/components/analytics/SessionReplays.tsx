import { getProjectSessionReplaysOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
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
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  Calendar,
  Clock,
  ExternalLink,
  Loader2,
  Monitor,
  Play,
  User,
  Video,
} from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import { TimeAgo } from '../utils/TimeAgo'

interface SessionReplaysProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
}

export function SessionReplays({
  project,
  startDate,
  endDate,
}: SessionReplaysProps) {
  const navigate = useNavigate()

  // Fetch session replays directly for the project
  const {
    data: replaysData,
    isLoading,
    error,
  } = useQuery({
    ...getProjectSessionReplaysOptions({
      query: {
        project_id: project.id,
        page: 1,
        per_page: 50,
      },
    }),
  })

  const handlePlayReplay = (replayId: string, visitorId: number) => {
    navigate(
      `/projects/${project.slug}/analytics/visitors/${visitorId}/session-replay/${replayId}`
    )
  }

  if (error) {
    return (
      <Card>
        <CardContent className="py-8">
          <div className="flex flex-col items-center justify-center text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load session replays
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        </CardContent>
      </Card>
    )
  }

  return (
    <div>
      {/* Sessions Table */}
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div className="space-y-1">
              <CardTitle>Session Replays</CardTitle>
              <CardDescription>
                {startDate && endDate
                  ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                  : 'Recent session recordings'}
              </CardDescription>
            </div>
            {isLoading && (
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                Loading sessions...
              </div>
            )}
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {isLoading ? (
            <div className="p-8">
              <div className="space-y-4">
                {[...Array(5)].map((_, i) => (
                  <div
                    key={i}
                    className="flex items-center justify-between p-4 border rounded-lg"
                  >
                    <div className="flex items-center gap-4">
                      <div className="h-10 w-10 bg-muted animate-pulse rounded-full" />
                      <div className="space-y-2">
                        <div className="h-4 w-32 bg-muted animate-pulse rounded" />
                        <div className="h-3 w-48 bg-muted animate-pulse rounded" />
                      </div>
                    </div>
                    <div className="h-8 w-20 bg-muted animate-pulse rounded" />
                  </div>
                ))}
              </div>
            </div>
          ) : !replaysData?.sessions || replaysData.sessions.length === 0 ? (
            <div className="p-8">
              <div className="flex flex-col items-center justify-center text-center">
                <div className="h-12 w-12 rounded-full bg-muted flex items-center justify-center mb-4">
                  <Video className="h-6 w-6 text-muted-foreground" />
                </div>
                <p className="text-sm font-medium">No session replays yet</p>
                <p className="text-sm text-muted-foreground mt-1">
                  Session replays will appear once users visit your application
                </p>
              </div>
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Visitor</TableHead>
                  <TableHead>Duration</TableHead>
                  <TableHead>Viewport</TableHead>
                  <TableHead>Created</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {replaysData.sessions.map((replay) => (
                  <TableRow key={replay.id}>
                    <TableCell>
                      <div className="flex items-center gap-1">
                        <User className="h-3 w-3 text-muted-foreground" />
                        <span className="text-sm">{replay.visitor_id}</span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1">
                        <Clock className="h-3 w-3 text-muted-foreground" />
                        <span className="text-sm">{replay.duration || 0}s</span>
                      </div>
                    </TableCell>

                    <TableCell>
                      <div className="flex items-center gap-1">
                        <Monitor className="h-3 w-3 text-muted-foreground" />
                        <span className="text-xs text-muted-foreground">
                          {replay.viewport_width}×{replay.viewport_height}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1">
                        <Calendar className="h-3 w-3 text-muted-foreground" />
                        <span className="text-xs text-muted-foreground">
                          <TimeAgo date={replay.created_at || ''} />
                        </span>
                      </div>
                    </TableCell>
                    <TableCell className="text-right">
                      <Button
                        size="sm"
                        onClick={() =>
                          handlePlayReplay(
                            replay.id.toString(),
                            replay.visitor_id
                          )
                        }
                        className="gap-2"
                      >
                        <Play className="h-3 w-3" />
                        Watch
                        <ExternalLink className="h-3 w-3" />
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

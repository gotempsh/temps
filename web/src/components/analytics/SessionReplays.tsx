// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectSessionReplaysOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse, SessionReplayWithVisitorDto } from '@/api/client/types.gen'
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
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  Calendar,
  Clock,
  Globe,
  Loader2,
  Monitor,
  Play,
  User,
  Video,
} from 'lucide-react'
import { useNavigate } from 'react-router'
import { TimeAgo } from '../utils/TimeAgo'

function formatLocation(replay: SessionReplayWithVisitorDto): string | null {
  const parts: string[] = []
  if (replay.visitor_city) parts.push(replay.visitor_city)
  if (replay.visitor_country) parts.push(replay.visitor_country)
  return parts.length > 0 ? parts.join(', ') : null
}

function formatBrowserInfo(replay: SessionReplayWithVisitorDto): string | null {
  if (!replay.browser) return null
  return replay.browser
}

function formatOsInfo(replay: SessionReplayWithVisitorDto): string | null {
  if (!replay.operating_system) return null
  return replay.operating_system
}

function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000)
  if (seconds === 0) return '< 1s'
  if (seconds < 60) return `${seconds}s`
  const mins = Math.floor(seconds / 60)
  const secs = seconds % 60
  if (mins < 60) return secs > 0 ? `${mins}m ${secs}s` : `${mins}m`
  const hrs = Math.floor(mins / 60)
  const remainMins = mins % 60
  return remainMins > 0 ? `${hrs}h ${remainMins}m` : `${hrs}h`
}

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
            <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Visitor</TableHead>
                  <TableHead className="hidden md:table-cell">Browser / OS</TableHead>
                  <TableHead className="hidden sm:table-cell">Location</TableHead>
                  <TableHead>Duration</TableHead>
                  <TableHead className="hidden lg:table-cell">Viewport</TableHead>
                  <TableHead className="hidden sm:table-cell">Created</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {replaysData.sessions.map((replay) => (
                  <TableRow key={replay.id}>
                    <TableCell>
                      <div className="flex items-center gap-1.5">
                        <User className="h-3 w-3 text-muted-foreground shrink-0" />
                        <span className="text-sm font-medium">{replay.visitor_id}</span>
                      </div>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <div className="flex flex-col gap-0.5">
                            {formatBrowserInfo(replay) ? (
                              <>
                                <span className="text-sm">{formatBrowserInfo(replay)}</span>
                                {formatOsInfo(replay) && (
                                  <span className="text-xs text-muted-foreground">{formatOsInfo(replay)}</span>
                                )}
                              </>
                            ) : (
                              <span className="text-xs text-muted-foreground">-</span>
                            )}
                          </div>
                        </TooltipTrigger>
                        {(replay.browser || replay.operating_system || replay.device_type) && (
                          <TooltipContent>
                            <div className="text-xs space-y-0.5">
                              {replay.browser && <div>Browser: {replay.browser} {replay.browser_version}</div>}
                              {replay.operating_system && <div>OS: {replay.operating_system} {replay.operating_system_version}</div>}
                              {replay.device_type && <div>Device: {replay.device_type}</div>}
                            </div>
                          </TooltipContent>
                        )}
                      </Tooltip>
                    </TableCell>
                    <TableCell className="hidden sm:table-cell">
                      {formatLocation(replay) ? (
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <div className="flex items-center gap-1.5">
                              <Globe className="h-3 w-3 text-muted-foreground shrink-0" />
                              <span className="text-sm">{formatLocation(replay)}</span>
                            </div>
                          </TooltipTrigger>
                          <TooltipContent>
                            <div className="text-xs space-y-0.5">
                              {replay.visitor_city && <div>City: {replay.visitor_city}</div>}
                              {replay.visitor_region && <div>Region: {replay.visitor_region}</div>}
                              {replay.visitor_country && <div>Country: {replay.visitor_country}</div>}
                            </div>
                          </TooltipContent>
                        </Tooltip>
                      ) : (
                        <span className="text-xs text-muted-foreground">-</span>
                      )}
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1">
                        <Clock className="h-3 w-3 text-muted-foreground" />
                        <span className="text-sm">{formatDuration(replay.duration || 0)}</span>
                      </div>
                    </TableCell>
                    <TableCell className="hidden lg:table-cell">
                      <div className="flex items-center gap-1">
                        <Monitor className="h-3 w-3 text-muted-foreground" />
                        <span className="text-xs text-muted-foreground">
                          {replay.viewport_width}x{replay.viewport_height}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell className="hidden sm:table-cell">
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
                        className="gap-1.5"
                      >
                        <Play className="h-3 w-3" />
                        <span className="hidden sm:inline">Watch</span>
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

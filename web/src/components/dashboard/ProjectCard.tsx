import { ProjectResponse } from '@/api/client'
import { getLastDeploymentOptions } from '@/api/client/@tanstack/react-query.gen'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent } from '@/components/ui/card'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { Skeleton } from '@/components/ui/skeleton'
import { ReloadableImage } from '@/components/utils/ReloadableImage'
import { TimeAgo } from '@/components/utils/TimeAgo'
import type { ProjectDashboardAnalytics } from '@/hooks/useDashboardAnalytics'
import type { ProjectMonitorHealth } from '@/hooks/useDashboardHealth'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, TrendingDown, TrendingUp, Minus } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import { VisitorSparkline } from './VisitorSparkline'

function formatTrend(trendPercentage: number | null | undefined): {
  label: string
  icon: React.ReactNode
  className: string
} | null {
  if (trendPercentage == null) return null

  const rounded = Math.round(trendPercentage)

  if (rounded === 0) {
    return {
      label: '0%',
      icon: <Minus className="h-3 w-3" />,
      className: 'text-muted-foreground',
    }
  }

  if (rounded > 0) {
    return {
      label: `+${rounded}%`,
      icon: <TrendingUp className="h-3 w-3" />,
      className: 'text-emerald-600 dark:text-emerald-400',
    }
  }

  return {
    label: `${rounded}%`,
    icon: <TrendingDown className="h-3 w-3" />,
    className: 'text-red-600 dark:text-red-400',
  }
}

interface ProjectCardProps {
  project: ProjectResponse
  shortcutNumber?: number
  /** Pre-fetched analytics data from the batch endpoint */
  analytics?: ProjectDashboardAnalytics
  /** Whether the batch analytics query is still loading */
  analyticsLoading?: boolean
  /** Whether the batch analytics query errored */
  analyticsError?: boolean
  /** Pre-fetched health data from the batch monitor endpoint */
  health?: ProjectMonitorHealth
}

function HealthStatusDot({ status }: { status: string }) {
  const colors: Record<string, string> = {
    operational: 'bg-emerald-500',
    degraded: 'bg-amber-500',
    down: 'bg-red-500',
    no_monitors: 'bg-zinc-400',
    unknown: 'bg-zinc-400',
  }
  return (
    <span
      className={`inline-block h-2 w-2 rounded-full ${colors[status] || colors.unknown}`}
      title={status === 'no_monitors' ? 'No monitors' : status.charAt(0).toUpperCase() + status.slice(1)}
    />
  )
}

export function ProjectCard({
  project,
  shortcutNumber,
  analytics,
  analyticsLoading = false,
  analyticsError = false,
  health,
}: ProjectCardProps) {
  // State for hover effect
  const [isHovering, setIsHovering] = useState(false)

  const totalVisitors = analytics?.unique_visitors ?? 0
  const hourlyData = analytics?.hourly_visits ?? []
  const trend = formatTrend(analytics?.trend_percentage)

  // Fetch last deployment to get screenshot
  const { data: lastDeployment } = useQuery({
    ...getLastDeploymentOptions({
      path: {
        id: project.id,
      },
    }),
    enabled: !!project.id,
    refetchOnWindowFocus: true,
  })

  return (
    <Link
      to={`/projects/${project.slug}`}
      onMouseEnter={() => setIsHovering(true)}
      onMouseLeave={() => setIsHovering(false)}
    >
      <Card className="hover:bg-muted/50 transition-colors">
        <CardContent className="p-4">
          <div className="flex items-start justify-between gap-4">
            {/* Left side: Avatar/Screenshot + Project info */}
            <div className="flex items-start gap-3 flex-1 min-w-0">
              {lastDeployment?.screenshot_location ? (
                <div className="size-10 flex-shrink-0 rounded-md overflow-hidden border bg-muted/30">
                  <ReloadableImage
                    src={`/api/files${lastDeployment.screenshot_location.startsWith('/') ? lastDeployment.screenshot_location : '/' + lastDeployment.screenshot_location}`}
                    alt={`${project.slug} preview`}
                    className="w-full h-full object-cover object-top"
                  />
                </div>
              ) : (
                <Avatar className="size-10 flex-shrink-0">
                  <AvatarImage src={`/api/projects/${project.id}/favicon`} />
                  <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
                </Avatar>
              )}
              <div className="space-y-0.5 flex-1 min-w-0">
                <div className="flex flex-col sm:flex-row sm:items-center sm:gap-2">
                  <h2 className="font-semibold leading-none truncate flex items-center gap-1.5">
                    {project.slug}
                    {health && health.status !== 'no_monitors' && (
                      <HealthStatusDot status={health.status} />
                    )}
                  </h2>
                  {!project.last_deployment && (
                    <Badge variant="outline" className="mt-1 w-fit sm:mt-0">
                      Not deployed
                    </Badge>
                  )}
                </div>
                {project.last_deployment && (
                  <p className="text-xs text-muted-foreground">
                    Deployed <TimeAgo date={project.last_deployment} />
                  </p>
                )}
              </div>
            </div>

            {/* Right side: Keyboard shortcut */}
            <div className="flex items-center gap-3 flex-shrink-0">
              {shortcutNumber !== undefined && (
                <KbdBadge keys={['⌃', shortcutNumber.toString()]} />
              )}
            </div>
          </div>

          {/* Analytics Section */}
          {analyticsLoading ? (
            <>
              <div className="mt-3 flex items-baseline gap-2">
                <Skeleton className="h-8 w-16" />
                <Skeleton className="h-4 w-12" />
                <span className="text-sm text-muted-foreground">
                  visitors in last 24h
                </span>
              </div>
              <div className="mt-2 h-[60px] w-full">
                <Skeleton className="h-full w-full" />
              </div>
            </>
          ) : analyticsError ? (
            <div className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
              <AlertCircle className="h-4 w-4" />
              <span>Unable to load analytics</span>
            </div>
          ) : (
            <>
              <div className="mt-3 flex items-baseline gap-2">
                <div className="text-2xl font-bold">{totalVisitors}</div>
                {trend && (
                  <span className={`inline-flex items-center gap-0.5 text-xs font-medium ${trend.className}`}>
                    {trend.icon}
                    {trend.label}
                  </span>
                )}
                <span className="text-sm text-muted-foreground">
                  visitors in last 24h
                </span>
              </div>

              <VisitorSparkline
                data={hourlyData.map((e) => ({
                  hour: e.date,
                  count: e.count,
                }))}
                className="mt-2 w-full"
                height={60}
                isHovering={isHovering}
              />
            </>
          )}

        </CardContent>
      </Card>
    </Link>
  )
}

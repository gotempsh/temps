import { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getErrorDashboardStatsOptions,
  getLastDeploymentOptions,
  getUniqueCountsOptions,
  hasAnalyticsEventsOptions,
  hasErrorGroupsOptions,
} from '@/api/client/@tanstack/react-query.gen'
// getProjectVisitorStatsOptions, getTodayErrorsCountOptions
import { LastDeployment } from '@/components/deployments/LastDeployment'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import {
  AlertCircle,
  Bug,
  DollarSign,
  Minus,
  TrendingDown,
  TrendingUp,
  Users,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { MetricCard } from '../dashboard/MetricCard'
import { DeploymentActivityGraph } from './DeploymentActivityGraph'

interface ProjectOverviewProps {
  project: ProjectResponse
  lastDeployment?: DeploymentResponse
}

function getChangeDisplay(change: number | undefined, inverse = false) {
  if (change === undefined)
    return {
      icon: <Minus className="h-4 w-4" />,
      className: 'text-muted-foreground',
    }
  if (change === 0)
    return {
      icon: <Minus className="h-4 w-4" />,
      className: 'text-muted-foreground',
    }

  const isPositive = inverse ? change < 0 : change > 0
  const showUpArrow = inverse ? change < 0 : change > 0

  return {
    icon: showUpArrow ? (
      <TrendingUp className="h-4 w-4" />
    ) : (
      <TrendingDown className="h-4 w-4" />
    ),
    className: cn(
      'flex items-center gap-1',
      isPositive ? 'text-emerald-600 dark:text-emerald-400' : 'text-destructive'
    ),
    isPositive,
  }
}

export function ProjectOverview({
  project,
  lastDeployment,
}: ProjectOverviewProps) {
  // Memoize dates to prevent unnecessary re-renders
  const { startDate, endDate } = useMemo(
    () => ({
      startDate: subDays(new Date(), 1),
      endDate: new Date(),
    }),
    []
  )

  const {
    data: visitorStats,
    isLoading: isLoadingVisitors,
    error: visitorError,
  } = useQuery({
    ...getUniqueCountsOptions({
      path: { project_id: project.id },
      query: {
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
        metric: 'visitors',
      },
    }),
    enabled: !!project.id,
  })
  const { data: errorStats } = useQuery({
    ...getErrorDashboardStatsOptions({
      query: {
        start_time: startDate.toISOString(),
        end_time: endDate.toISOString(),
        compare_to_previous: true,
      },
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })
  const isLoadingErrors = false
  const errorError = false

  // Check if analytics and error tracking are configured
  const { data: hasAnalyticsData, isLoading: isCheckingAnalytics } = useQuery({
    ...hasAnalyticsEventsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  const { data: hasErrorsData, isLoading: isCheckingErrors } = useQuery({
    ...hasErrorGroupsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  // Query for fresh deployment data with polling when needed
  const { data: freshLastDeployment, refetch: refetchDeployment } = useQuery({
    ...getLastDeploymentOptions({
      path: {
        id: project.id || 0,
      },
    }),
    enabled: !!project.id,
    refetchInterval: (query) => {
      const data = query.state.data
      // Keep polling while deployment is in progress
      if (
        !data ||
        data.status === 'pending' ||
        data.status === 'running' ||
        data.status === 'building'
      ) {
        return 2500 // 2.5 seconds
      }
      // Also poll if deployment is completed but screenshot is not yet available
      if (data.status === 'completed' && !data.screenshot_location) {
        return 3000 // 3 seconds while waiting for screenshot
      }
      return false // Stop polling when deployment has screenshot or failed
    },
  })

  // Use fresh deployment data if available, otherwise fall back to passed prop
  const currentDeployment = freshLastDeployment || lastDeployment

  // Refresh deployment data when component mounts
  useEffect(() => {
    if (project?.id) {
      refetchDeployment()
    }
  }, [project?.id, refetchDeployment])

  // Use useState with lazy initialization for timestamps to avoid impure function calls during render
  const [now] = useState(() => Date.now())
  const [oneDayAgo] = useState(() => now - 24 * 60 * 60 * 1000)

  // Determine what's not configured
  const missingAnalytics = !isCheckingAnalytics && !hasAnalyticsData?.has_events
  const missingErrorTracking =
    !isCheckingErrors && !hasErrorsData?.has_error_groups

  return (
    <>
      {/* Configuration Alert */}
      {(missingAnalytics || missingErrorTracking) && (
        <Alert variant="default" className="mb-6 border-amber-500/50 bg-amber-50/50 dark:bg-amber-950/20">
          <AlertCircle className="h-4 w-4 text-amber-600 dark:text-amber-500" />
          <AlertTitle className="text-amber-900 dark:text-amber-100">
            Complete Your Setup
          </AlertTitle>
          <AlertDescription className="text-amber-800 dark:text-amber-200">
            <p className="mb-3">
              To get the most out of your project, please complete the following:
            </p>
            <div className="flex flex-col sm:flex-row gap-2">
              {missingAnalytics && (
                <Button
                  asChild
                  variant="outline"
                  size="sm"
                  className="border-amber-600 hover:bg-amber-100 dark:hover:bg-amber-900/30"
                >
                  <Link to={`/projects/${project.slug}/analytics/setup`}>
                    Set up Analytics
                  </Link>
                </Button>
              )}
              {missingErrorTracking && (
                <Button
                  asChild
                  variant="outline"
                  size="sm"
                  className="border-amber-600 hover:bg-amber-100 dark:hover:bg-amber-900/30"
                >
                  <Link to={`/projects/${project.slug}/errors`}>
                    Set up Error Tracking
                  </Link>
                </Button>
              )}
            </div>
          </AlertDescription>
        </Alert>
      )}

      <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
        {isLoadingVisitors ? (
          <Skeleton className="h-24" />
        ) : visitorError ? (
          <MetricCard
            title="Visitors last 24 hours (Unique)"
            icon={<Users />}
            value="Error"
            change=""
            error={true}
          />
        ) : (
          <MetricCard
            change=""
            changeDisplay={getChangeDisplay(
              Number((visitorStats?.count || 0).toFixed(1))
            )}
            value={visitorStats?.count || '0'}
            title="Visitors last 24 hours"
            icon={<Users />}
          />
        )}

        {/* Revenue - Coming Soon */}
        <div className="relative">
          <MetricCard
            title="Revenue"
            value="$0"
            change=""
            icon={<DollarSign className="h-5 w-5" />}
          />
          <div className="absolute inset-0 bg-background/80 rounded-lg flex items-center justify-center">
            <Badge variant="secondary" className="text-xs">
              Coming Soon
            </Badge>
          </div>
        </div>

        {isLoadingErrors ? (
          <Skeleton className="h-24" />
        ) : errorError ? (
          <MetricCard
            title="Errors"
            icon={<Bug />}
            value="Error"
            change=""
            error={true}
          />
        ) : (
          <Link
            to={`/projects/${project.slug}/analytics/requests?from=${oneDayAgo}&to=${now}&status_code=500`}
            className="w-full h-full"
          >
            <MetricCard
              change={''}
              value={errorStats?.error_groups?.toFixed(2) || '0'}
              title="Errors"
              icon={<Bug />}
            />
          </Link>
        )}
      </div>
      <div className="mt-4">
        {currentDeployment && (
          <LastDeployment
            deployment={currentDeployment}
            projectName={project.slug}
          />
        )}
      </div>
      <div className="mt-6">
        <DeploymentActivityGraph projectId={project.id} />
      </div>
    </>
  )
}

import { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getErrorDashboardStatsOptions,
  getLastDeploymentOptions,
  getUniqueCountsOptions,
} from '@/api/client/@tanstack/react-query.gen'
// getProjectVisitorStatsOptions, getTodayErrorsCountOptions
import { LastDeployment } from '@/components/deployments/LastDeployment'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import {
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
      if (
        !data ||
        data.status === 'failed' ||
        data.status === 'pending' ||
        data.status === 'building'
      ) {
        return 2500 // 2.5 seconds
      }
      return false // Stop polling when deployment is successful
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

  return (
    <>
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

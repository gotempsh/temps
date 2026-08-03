import { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getErrorDashboardStatsOptions,
  getLastDeploymentOptions,
  getUniqueCountsOptions,
  revenueListIntegrationsOptions,
  revenueMetricsSummaryOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
  Sparkles,
  TrendingDown,
  TrendingUp,
  Users,
} from 'lucide-react'
import { useEffect, useMemo } from 'react'
import { Link } from 'react-router'
import { MetricCard } from '../dashboard/MetricCard'
import { DeploymentActivityGraph } from './DeploymentActivityGraph'
import { PROJECT_TOUR_EVENT } from './ProjectTour'

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

  const { data: freshLastDeployment, refetch: refetchDeployment } = useQuery({
    ...getLastDeploymentOptions({
      path: {
        id: project.id || 0,
      },
    }),
    enabled: !!project.id,
    refetchInterval: (query) => {
      const data = query.state.data as any
      if (
        !data ||
        data.status === 'pending' ||
        data.status === 'running' ||
        data.status === 'building'
      ) {
        return 2500
      }
      if (data.status === 'completed' && !data.screenshot_location) {
        return 3000
      }
      return false
    },
    refetchOnWindowFocus: true,
  })

  const currentDeployment = freshLastDeployment || lastDeployment

  useEffect(() => {
    if (project?.id) {
      refetchDeployment()
    }
  }, [project?.id, refetchDeployment])

  return (
    <>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 sm:gap-4 lg:grid-cols-3 lg:gap-6">
        {isLoadingVisitors ? (
          <Skeleton className="h-24" />
        ) : visitorError ? (
          <Link
            to={`/projects/${project.slug}/analytics`}
            className="h-full w-full"
          >
            <MetricCard
              title="Visitors last 24 hours (Unique)"
              icon={<Users />}
              value="Error"
              change=""
              error={true}
            />
          </Link>
        ) : (
          <Link
            to={`/projects/${project.slug}/analytics`}
            className="h-full w-full"
          >
            <MetricCard
              change=""
              changeDisplay={getChangeDisplay(
                Number((visitorStats?.count || 0).toFixed(1))
              )}
              value={visitorStats?.count || '0'}
              title="Visitors last 24 hours"
              icon={<Users />}
            />
          </Link>
        )}

        <RevenueMetric project={project} />

        <Link to={`/projects/${project.slug}/errors`} className="h-full w-full">
          <MetricCard
            change={''}
            value={errorStats?.error_groups?.toFixed(2) || '0'}
            title="Errors"
            icon={<Bug />}
          />
        </Link>
      </div>

      <div className="mt-4 sm:mt-6">
        {currentDeployment && (
          <LastDeployment
            deployment={currentDeployment}
            projectName={project.slug}
          />
        )}
      </div>

      <div className="mt-4 sm:mt-6">
        <DeploymentActivityGraph projectId={project.id} />
      </div>

      <div className="mt-4 flex justify-center sm:mt-6">
        <button
          type="button"
          onClick={() => window.dispatchEvent(new Event(PROJECT_TOUR_EVENT))}
          className="inline-flex items-center gap-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground"
        >
          <Sparkles className="size-3.5" />
          Take a tour of your project
        </button>
      </div>
    </>
  )
}

function RevenueMetric({ project }: { project: ProjectResponse }) {
  const integrationsQuery = useQuery({
    ...revenueListIntegrationsOptions({ path: { project_id: project.id } }),
  })
  const hasIntegrations = (integrationsQuery.data?.length ?? 0) > 0

  const summaryQuery = useQuery({
    ...revenueMetricsSummaryOptions({ path: { project_id: project.id } }),
    enabled: hasIntegrations,
  })

  if (!hasIntegrations) {
    return (
      <Link to={`/projects/${project.slug}/revenue`} className="h-full w-full">
        <div className="relative">
          <MetricCard
            title="Revenue"
            value="Connect"
            change=""
            icon={<DollarSign className="h-5 w-5" />}
          />
          <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-background/60">
            <Badge variant="secondary" className="text-xs">
              Connect a provider
            </Badge>
          </div>
        </div>
      </Link>
    )
  }

  const mrr = summaryQuery.data?.current_mrr_minor ?? 0
  const currency = summaryQuery.data?.currency ?? 'usd'
  const display = (() => {
    try {
      return new Intl.NumberFormat(undefined, {
        style: 'currency',
        currency: currency.toUpperCase(),
        maximumFractionDigits: 0,
      }).format(mrr / 100)
    } catch {
      return `${(mrr / 100).toFixed(0)} ${currency.toUpperCase()}`
    }
  })()

  return (
    <Link to={`/projects/${project.slug}/revenue`} className="h-full w-full">
      <MetricCard
        title="MRR"
        value={summaryQuery.isLoading ? '…' : display}
        change=""
        icon={<DollarSign className="h-5 w-5" />}
      />
    </Link>
  )
}

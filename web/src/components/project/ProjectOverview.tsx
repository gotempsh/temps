import { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getErrorDashboardStatsOptions,
  getLastDeploymentOptions,
  getUniqueCountsOptions,
  hasAnalyticsEventsOptions,
  hasErrorGroupsOptions,
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
  BarChart3,
  Bug,
  Check,
  ChevronRight,
  Circle,
  DollarSign,
  Minus,
  TrendingDown,
  TrendingUp,
  Users,
} from 'lucide-react'
import { ReactNode, useEffect, useMemo } from 'react'
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

type OnboardingStepId = 'analytics' | 'errors'

interface OnboardingStep {
  id: OnboardingStepId
  title: string
  description: string
  href: string
  done: boolean
  icon: ReactNode
  estimate: string
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

  const isLoadingOnboarding = isCheckingAnalytics || isCheckingErrors
  const hasAnalytics = !!hasAnalyticsData?.has_events
  const hasErrors = !!hasErrorsData?.has_error_groups

  const steps: OnboardingStep[] = [
    {
      id: 'analytics',
      title: 'Install analytics SDK',
      description:
        'Send your first pageview to unlock visitors, pages, and funnels.',
      href: `/projects/${project.slug}/analytics/setup`,
      done: hasAnalytics,
      icon: <BarChart3 className="size-4" />,
      estimate: '3 min',
    },
    {
      id: 'errors',
      title: 'Install error tracking SDK',
      description:
        'Capture your first exception to unlock stack traces, alerts, and autofix.',
      href: `/projects/${project.slug}/errors/setup`,
      done: hasErrors,
      icon: <Bug className="size-4" />,
      estimate: '3 min',
    },
  ]

  const doneCount = steps.filter((s) => s.done).length
  const totalCount = steps.length
  const percent = Math.round((doneCount / totalCount) * 100)
  const allDone = doneCount === totalCount

  return (
    <>
      {!isLoadingOnboarding && !allDone && (
        <section className="mb-4 overflow-hidden rounded-xl border bg-card sm:mb-6">
          <div className="flex flex-col gap-3 border-b p-4 sm:flex-row sm:items-center sm:justify-between sm:gap-4 sm:p-5">
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <h2 className="text-base font-semibold tracking-tight">
                  Finish setting up {project.slug}
                </h2>
                <Badge variant="secondary" className="tabular-nums">
                  {doneCount} / {totalCount}
                </Badge>
              </div>
              <p className="mt-1 text-sm text-muted-foreground">
                Wire up observability so Temps can start capturing data from
                your app.
              </p>
            </div>
            <div className="flex items-center gap-3 sm:w-64 sm:shrink-0">
              <div className="h-2 flex-1 overflow-hidden rounded-full bg-muted">
                <div
                  className="h-full bg-primary [transition-duration:400ms] transition-all"
                  style={{ width: `${percent}%` }}
                />
              </div>
              <span className="text-sm font-medium tabular-nums text-muted-foreground">
                {percent}%
              </span>
            </div>
          </div>
          <ul role="list" className="divide-y">
            {steps.map((step) => (
              <li key={step.id}>
                <Link
                  to={step.href}
                  className={cn(
                    'group flex items-center gap-3 px-4 py-3 transition-colors hover:bg-muted/50 sm:gap-4 sm:px-5 sm:py-4',
                    step.done && 'opacity-60'
                  )}
                >
                  <span
                    className={cn(
                      'flex size-7 shrink-0 items-center justify-center rounded-full border',
                      step.done
                        ? 'border-emerald-500 bg-emerald-500 text-white'
                        : 'border-muted-foreground/30 text-muted-foreground'
                    )}
                  >
                    {step.done ? (
                      <Check className="size-4" strokeWidth={3} />
                    ) : (
                      <Circle className="size-4" />
                    )}
                  </span>
                  <div className="flex-1 min-w-0">
                    <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                      <p
                        className={cn(
                          'text-sm font-medium',
                          step.done && 'line-through'
                        )}
                      >
                        {step.title}
                      </p>
                      {!step.done && (
                        <span className="text-xs text-muted-foreground tabular-nums">
                          · {step.estimate}
                        </span>
                      )}
                    </div>
                    <p className="mt-0.5 text-sm text-muted-foreground">
                      {step.description}
                    </p>
                  </div>
                  {!step.done && (
                    <ChevronRight className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
                  )}
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}

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

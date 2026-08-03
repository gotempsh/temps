import { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getErrorDashboardStatsOptions,
  getLastDeploymentOptions,
  getUniqueCountsOptions,
  hasAnalyticsEventsOptions,
  hasErrorGroupsOptions,
  listCustomDomainsForProjectOptions,
  listMonitorsOptions,
  listProjectServicesOptions,
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
  Activity,
  BarChart3,
  Bug,
  ChevronRight,
  Database,
  DollarSign,
  Globe,
  Minus,
  Sparkles,
  TrendingDown,
  TrendingUp,
  Users,
} from 'lucide-react'
import { ReactNode, useEffect, useMemo } from 'react'
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

type OnboardingStepId =
  'analytics' | 'errors' | 'domain' | 'monitoring' | 'storage'

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

  const { data: customDomainsData, isLoading: isCheckingDomain } = useQuery({
    ...listCustomDomainsForProjectOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  const { data: monitorsData, isLoading: isCheckingMonitoring } = useQuery({
    ...listMonitorsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  const { data: servicesLinkedData, isLoading: isCheckingStorage } = useQuery({
    ...listProjectServicesOptions({
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

  const isLoadingOnboarding =
    isCheckingAnalytics ||
    isCheckingErrors ||
    isCheckingDomain ||
    isCheckingMonitoring ||
    isCheckingStorage
  const hasAnalytics = !!hasAnalyticsData?.has_events
  const hasErrors = !!hasErrorsData?.has_error_groups
  const hasDomain = (customDomainsData?.domains?.length ?? 0) > 0
  const hasMonitoring = (monitorsData?.length ?? 0) > 0
  const hasStorage = (servicesLinkedData?.length ?? 0) > 0

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
    {
      id: 'domain',
      title: 'Connect a custom domain',
      description: 'Point your own domain at this project with auto TLS.',
      href: `/projects/${project.slug}/domains`,
      done: hasDomain,
      icon: <Globe className="size-4" />,
      estimate: '2 min',
    },
    {
      id: 'monitoring',
      title: 'Add an uptime monitor',
      description: 'Get alerted the moment your app stops responding.',
      href: `/projects/${project.slug}/monitors`,
      done: hasMonitoring,
      icon: <Activity className="size-4" />,
      estimate: '1 min',
    },
    {
      id: 'storage',
      title: 'Link a database or storage service',
      description:
        'Attach a managed Postgres, Redis, or S3-compatible service.',
      href: `/projects/${project.slug}/storage`,
      done: hasStorage,
      icon: <Database className="size-4" />,
      estimate: '2 min',
    },
  ]

  const doneCount = steps.filter((s) => s.done).length
  const totalCount = steps.length
  const percent = Math.round((doneCount / totalCount) * 100)
  const allDone = doneCount === totalCount
  const remainingSteps = steps.filter((step) => !step.done)

  return (
    <div
      className={cn(
        !isLoadingOnboarding &&
          !allDone &&
          'grid gap-4 sm:gap-6 xl:grid-cols-[17rem_minmax(0,1fr)]'
      )}
    >
      {!isLoadingOnboarding && !allDone && (
        <aside aria-labelledby="project-setup-heading">
          <section className="overflow-hidden rounded-xl border bg-card">
            <div className="p-4">
              <div className="flex items-center justify-between gap-3">
                <h2
                  id="project-setup-heading"
                  className="text-sm font-semibold tracking-tight"
                >
                  Finish setup
                </h2>
                <span className="text-xs tabular-nums text-muted-foreground">
                  {doneCount} of {totalCount}
                </span>
              </div>
              <div
                className="mt-3 h-1 overflow-hidden rounded-full bg-muted"
                role="progressbar"
                aria-label="Project setup progress"
                aria-valuemin={0}
                aria-valuemax={totalCount}
                aria-valuenow={doneCount}
              >
                <div
                  className="h-full rounded-full bg-primary transition-all [transition-duration:400ms]"
                  style={{ width: `${percent}%` }}
                />
              </div>
            </div>

            <ul role="list" className="border-t p-2">
              {remainingSteps.map((step) => (
                <li key={step.id}>
                  <Link
                    to={step.href}
                    className="group flex items-center gap-3 rounded-lg px-2 py-2.5 transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border bg-background text-muted-foreground transition-colors group-hover:text-foreground">
                      {step.icon}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="block text-sm font-medium leading-tight">
                        {step.title}
                      </span>
                      <span className="mt-0.5 block text-xs tabular-nums text-muted-foreground">
                        {step.estimate}
                      </span>
                      <span className="sr-only">{step.description}</span>
                    </span>
                    <ChevronRight className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
                  </Link>
                </li>
              ))}
            </ul>

            {doneCount > 0 && (
              <p className="border-t px-4 py-3 text-xs text-muted-foreground">
                {doneCount} {doneCount === 1 ? 'step' : 'steps'} completed
              </p>
            )}
          </section>
        </aside>
      )}

      <div className="min-w-0">
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

          <Link
            to={`/projects/${project.slug}/errors`}
            className="h-full w-full"
          >
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
      </div>
    </div>
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

import { getUniqueCountsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'

import { useQuery } from '@tanstack/react-query'
import { Users, UserRoundCheck, MousePointer, FileText } from 'lucide-react'

interface AnalyticsMetricsProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function AnalyticsMetrics({
  project,
  startDate,
  endDate,
  environment,
}: AnalyticsMetricsProps) {
  // Fetch unique visitors
  const visitorsQuery = useQuery({
    ...getUniqueCountsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        metric: 'visitors',
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  // Fetch unique sessions
  const sessionsQuery = useQuery({
    ...getUniqueCountsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        metric: 'sessions',
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  // Fetch visitors who were seen before the selected range
  const returningVisitorsQuery = useQuery({
    ...getUniqueCountsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        metric: 'returning_visitors',
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  // Fetch unique paths
  const pathsQuery = useQuery({
    ...getUniqueCountsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        environment_id: environment,
        metric: 'paths',
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const isLoading =
    visitorsQuery.isLoading ||
    returningVisitorsQuery.isLoading ||
    sessionsQuery.isLoading ||
    pathsQuery.isLoading
  const hasError =
    visitorsQuery.error ||
    returningVisitorsQuery.error ||
    sessionsQuery.error ||
    pathsQuery.error

  const metrics = [
    {
      label: 'Unique Visitors',
      value: visitorsQuery.data?.count ?? 0,
      icon: Users,
      description: 'Total unique visitors',
    },
    {
      label: 'Returning Visitors',
      value: returningVisitorsQuery.data?.count ?? 0,
      icon: UserRoundCheck,
      description: 'Visitors seen before this period',
    },
    {
      label: 'Total Sessions',
      value: sessionsQuery.data?.count ?? 0,
      icon: MousePointer,
      description: 'Total number of sessions',
    },
    {
      label: 'Unique Pages',
      value: pathsQuery.data?.count ?? 0,
      icon: FileText,
      description: 'Total unique pages visited',
    },
  ]

  const gridCols = 'grid-cols-2 lg:grid-cols-4'
  const skeletonCount = 4

  return (
    <>
      {isLoading ? (
        <div className={`grid ${gridCols} gap-3 sm:gap-4`}>
          {[...Array(skeletonCount)].map((_, i) => (
            <div key={i} className="space-y-2 p-3 sm:p-4 rounded-lg border">
              <div className="h-4 w-20 bg-muted animate-pulse rounded" />
              <div className="h-8 w-16 bg-muted animate-pulse rounded" />
            </div>
          ))}
        </div>
      ) : hasError ? (
        <div className="flex flex-col items-center justify-center py-8 text-center">
          <p className="text-sm text-muted-foreground">
            Failed to load analytics metrics
          </p>
        </div>
      ) : (
        <div className={`grid ${gridCols} gap-3 sm:gap-4`}>
          {metrics.map((metric: any) => {
            const Icon = metric.icon
            const isComingSoon = metric.isComingSoon
            return (
              <div
                key={metric.label}
                className={`flex flex-col p-3 sm:p-4 rounded-lg border bg-card transition-colors ${
                  isComingSoon
                    ? 'border-dashed border-muted-foreground/30'
                    : 'hover:bg-accent/50'
                }`}
              >
                <div className="flex items-center justify-between mb-1 sm:mb-2">
                  <Icon
                    className={`h-4 w-4 sm:h-5 sm:w-5 ${isComingSoon ? 'text-muted-foreground/50' : 'text-muted-foreground'}`}
                  />
                </div>
                <div className="space-y-0.5 sm:space-y-1">
                  <p
                    className={`text-xl sm:text-2xl font-bold ${isComingSoon ? 'text-muted-foreground/50' : ''}`}
                  >
                    {typeof metric.value === 'number'
                      ? metric.value.toLocaleString()
                      : metric.value}
                  </p>
                  <p className="text-xs sm:text-sm text-muted-foreground">
                    {metric.label}
                  </p>
                </div>
              </div>
            )
          })}
        </div>
      )}
    </>
  )
}

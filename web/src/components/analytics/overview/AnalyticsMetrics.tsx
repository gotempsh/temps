// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { MetricSummary } from '@/components/data-display/MetricSummary'
import { Button } from '@/components/ui/button'

import { getUniqueCountsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'

import { useQuery } from '@tanstack/react-query'

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

  const uniqueVisitors = visitorsQuery.data?.count ?? 0
  const returningVisitors = returningVisitorsQuery.data?.count ?? 0
  const returningPercentage =
    uniqueVisitors > 0
      ? Math.round((returningVisitors / uniqueVisitors) * 100)
      : 0

  if (hasError)
    return (
      <div role="alert" className="rounded-lg border p-4 text-sm">
        <p>Could not load analytics metrics.</p>
        <Button
          variant="outline"
          size="sm"
          className="mt-2"
          onClick={() => {
            void visitorsQuery.refetch()
            void sessionsQuery.refetch()
            void pathsQuery.refetch()
            void returningVisitorsQuery.refetch()
          }}
        >
          Retry metrics
        </Button>
      </div>
    )

  return (
    <MetricSummary
      loading={isLoading}
      metrics={[
        {
          label: 'Visitors',
          value: uniqueVisitors,
          hint: `${returningPercentage}% returning`,
        },
        { label: 'Sessions', value: sessionsQuery.data?.count ?? 0 },
        { label: 'Pages', value: pathsQuery.data?.count ?? 0 },
      ]}
    />
  )
}

import {
  getHourlyVisitsOptions,
  getPropertyBreakdownOptions,
  getUniqueCountsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client/types.gen'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import {
  deriveBreakdownInsights,
  deriveReturnRateInsight,
  deriveTimingInsights,
} from './derive'
import { InsightsPanel } from './InsightsPanel'
import type { AiInsightContext, Insight } from './types'

interface OverviewInsightsProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

/** How many stat insights the overview shows before it becomes a wall. */
const MAX_OVERVIEW_INSIGHTS = 5

/**
 * Insights for the analytics overview page. Every query below uses the
 * exact same options as the chart that already renders that data
 * (VisitorChart, ChannelsChart, LocationsChart, AnalyticsMetrics), so
 * React Query deduplicates them — this component adds no extra requests.
 */
export function OverviewInsights({
  project,
  startDate,
  endDate,
  environment,
}: OverviewInsightsProps) {
  const enabled = !!startDate && !!endDate
  const baseQuery = {
    start_date: startDate ? startDate.toISOString() : '',
    end_date: endDate ? endDate.toISOString() : '',
    environment_id: environment,
  }

  const visitorsQuery = useQuery({
    ...getUniqueCountsOptions({
      path: { project_id: project.id },
      query: { ...baseQuery, metric: 'visitors' },
    }),
    enabled,
  })
  const sessionsQuery = useQuery({
    ...getUniqueCountsOptions({
      path: { project_id: project.id },
      query: { ...baseQuery, metric: 'sessions' },
    }),
    enabled,
  })
  const hourlyQuery = useQuery({
    ...getHourlyVisitsOptions({
      path: { project_id: project.id },
      query: { ...baseQuery, aggregation_level: 'visitors' },
    }),
    enabled,
  })
  const channelsQuery = useQuery({
    ...getPropertyBreakdownOptions({
      path: { project_id: project.id },
      query: {
        ...baseQuery,
        group_by: 'channel',
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled,
  })
  const countriesQuery = useQuery({
    ...getPropertyBreakdownOptions({
      path: { project_id: project.id },
      query: {
        ...baseQuery,
        group_by: 'country',
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled,
  })

  const isLoading =
    visitorsQuery.isLoading ||
    sessionsQuery.isLoading ||
    hourlyQuery.isLoading ||
    channelsQuery.isLoading ||
    countriesQuery.isLoading

  const channelRows = useMemo(
    () =>
      (channelsQuery.data?.items ?? []).map((item) => ({
        value: item.value || 'Direct',
        count: item.count,
      })),
    [channelsQuery.data]
  )
  const countryRows = useMemo(
    () =>
      (countriesQuery.data?.items ?? []).map((item) => ({
        value: item.value || 'Unknown',
        count: item.count,
      })),
    [countriesQuery.data]
  )

  const insights = useMemo<Insight[]>(() => {
    const timing = deriveTimingInsights(hourlyQuery.data ?? [], 'visitors')
    const returnRate = deriveReturnRateInsight(
      visitorsQuery.data?.count ?? 0,
      sessionsQuery.data?.count ?? 0
    )
    const channels = deriveBreakdownInsights({
      rows: channelRows,
      singular: 'channel',
      plural: 'channels',
      flavor: 'acquisition',
    })
    const countries = deriveBreakdownInsights({
      rows: countryRows,
      singular: 'country',
      plural: 'countries',
      flavor: 'geo',
    })
    return [
      ...timing.filter((i) => i.id === 'stat-trend'),
      ...(returnRate ? [returnRate] : []),
      ...channels,
      ...countries.slice(0, 1),
      ...timing.filter((i) => i.id === 'stat-peak-time'),
    ].slice(0, MAX_OVERVIEW_INSIGHTS)
  }, [
    hourlyQuery.data,
    visitorsQuery.data,
    sessionsQuery.data,
    channelRows,
    countryRows,
  ])

  const aiContext = useMemo<AiInsightContext | undefined>(() => {
    if (isLoading || (visitorsQuery.data?.count ?? 0) === 0) return undefined

    // Compact the hourly series into per-day totals so the prompt stays small.
    const dailyVisitors: Record<string, number> = {}
    for (const point of hourlyQuery.data ?? []) {
      const day = point.date.slice(0, 10)
      dailyVisitors[day] = (dailyVisitors[day] ?? 0) + point.count
    }

    return {
      surface: 'web analytics overview',
      rangeStart: startDate?.toISOString(),
      rangeEnd: endDate?.toISOString(),
      stats: {
        unique_visitors: visitorsQuery.data?.count ?? 0,
        total_sessions: sessionsQuery.data?.count ?? 0,
        top_channels: channelRows.slice(0, 5),
        top_countries: countryRows.slice(0, 5),
        daily_visitors: dailyVisitors,
      },
    }
  }, [
    isLoading,
    visitorsQuery.data,
    sessionsQuery.data,
    hourlyQuery.data,
    channelRows,
    countryRows,
    startDate,
    endDate,
  ])

  return (
    <InsightsPanel
      insights={insights}
      isLoading={isLoading}
      aiContext={aiContext}
      project={{ id: project.id, slug: project.slug, name: project.name }}
    />
  )
}

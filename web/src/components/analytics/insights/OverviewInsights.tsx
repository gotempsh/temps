// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getHourlyVisitsOptions,
  getPropertyBreakdownOptions,
  getUniqueCountsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client/types.gen'
import { useMemo } from 'react'
import {
  deriveBreakdownInsights,
  deriveReturnRateInsight,
  deriveTimingInsights,
} from './derive'
import { InsightsPanel } from './InsightsPanel'
import type { AiInsightContext, Insight } from './types'
import { useWindowQuery } from '@/hooks/useWindowQuery'
import { sameWindow } from '@/lib/refresh'

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

  const visitorsOptions = getUniqueCountsOptions({
    path: { project_id: project.id },
    query: { ...baseQuery, metric: 'visitors' },
  })
  const visitorsQuery = useWindowQuery({ ...visitorsOptions, enabled })
  const sessionsOptions = getUniqueCountsOptions({
    path: { project_id: project.id },
    query: { ...baseQuery, metric: 'sessions' },
  })
  const sessionsQuery = useWindowQuery({ ...sessionsOptions, enabled })
  const hourlyQuery = useWindowQuery({
    ...getHourlyVisitsOptions({
      path: { project_id: project.id },
      query: { ...baseQuery, aggregation_level: 'visitors' },
    }),
    enabled,
  })
  const channelsQuery = useWindowQuery({
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
  const countriesQuery = useWindowQuery({
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
  // While a moved window loads, some of these still hold the previous
  // window's data; the AI must not get a mix of two windows under one label.
  const isPreviousWindow =
    visitorsQuery.isPlaceholderData ||
    sessionsQuery.isPlaceholderData ||
    hourlyQuery.isPlaceholderData ||
    channelsQuery.isPlaceholderData ||
    countriesQuery.isPlaceholderData

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

  // Sessions per visitor only from counts of one window, never the new
  // window's visitors over the previous window's sessions.
  const countsShareWindow = sameWindow(
    [visitorsOptions.queryKey, visitorsQuery],
    [sessionsOptions.queryKey, sessionsQuery]
  )

  const insights = useMemo<Insight[]>(() => {
    const timing = deriveTimingInsights(hourlyQuery.data ?? [], 'visitors')
    const returnRate = countsShareWindow
      ? deriveReturnRateInsight(
          visitorsQuery.data?.count ?? 0,
          sessionsQuery.data?.count ?? 0
        )
      : null
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
    countsShareWindow,
    visitorsQuery.data,
    sessionsQuery.data,
    channelRows,
    countryRows,
  ])

  const aiContext = useMemo<AiInsightContext | undefined>(() => {
    if (isLoading || isPreviousWindow) return undefined
    if ((visitorsQuery.data?.count ?? 0) === 0) return undefined

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
    isPreviousWindow,
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
      aiPending={isPreviousWindow}
      project={{ id: project.id, slug: project.slug, name: project.name }}
    />
  )
}

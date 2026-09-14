// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { pageSparkline } from '@/lib/page-sparkline'
import type { PagePathSparkline, ProjectResponse } from '@/api/client'
import {
  type ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { Clock, ExternalLink, Users } from 'lucide-react'
import { useMemo } from 'react'
import { Link, useSearchParams } from 'react-router'
import { Bar, BarChart, XAxis, YAxis } from 'recharts'

interface PageListItemProps {
  pagePath: string
  sessions: number
  avgTime: number
  project: ProjectResponse
  startDate?: Date
  endDate?: Date
  sparkline?: PagePathSparkline
}

const chartConfig = {
  sessions: {
    label: 'Sessions',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

export function PageListItem({
  pagePath,
  sessions,
  avgTime,
  project,
  sparkline,
  startDate,
  endDate,
}: PageListItemProps) {
  const [searchParams] = useSearchParams()

  const chartData = useMemo(() => {
    if (!sparkline || !startDate || !endDate) return []
    return pageSparkline(sparkline.points, startDate, endDate)
  }, [sparkline, startDate, endDate])

  const pageDetailUrl = useMemo(() => {
    const base = `/projects/${project.slug}/analytics/pages?path=${encodeURIComponent(pagePath)}`
    const filter = searchParams.get('filter')
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    const extra = [
      filter ? `filter=${filter}` : '',
      from ? `from=${from}` : '',
      to ? `to=${to}` : '',
    ]
      .filter(Boolean)
      .join('&')
    return extra ? `${base}&${extra}` : base
  }, [project.slug, pagePath, searchParams])

  return (
    <div className="group relative flex items-center gap-4 p-4 hover:bg-muted/50 transition-colors border-b last:border-b-0">
      {/* Page Info */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 mb-2">
          <Link
            to={pageDetailUrl}
            className="font-medium text-sm hover:text-primary transition-colors truncate"
          >
            {pagePath}
          </Link>
          <ExternalLink className="h-3 w-3 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity" />
        </div>

        <div className="flex items-center gap-4 text-xs text-muted-foreground">
          <div className="flex items-center gap-1">
            <Users className="h-3 w-3" />
            <span>{sessions.toLocaleString()} sessions</span>
          </div>
          <div className="flex items-center gap-1">
            <Clock className="h-3 w-3" />
            <span>{avgTime}s avg</span>
          </div>
        </div>
      </div>

      {/* Mini Chart */}
      <div className="w-24 h-10">
        {chartData.length > 0 ? (
          <ChartContainer
            config={chartConfig}
            className="h-full w-full aspect-auto"
          >
            <BarChart
              data={chartData}
              margin={{ top: 2, right: 2, bottom: 2, left: 2 }}
            >
              <Bar
                dataKey="sessions"
                fill="var(--color-sessions)"
                radius={[2, 2, 0, 0]}
                isAnimationActive={false}
              />
              <XAxis dataKey="time" hide />
              <YAxis domain={[0, 'auto']} allowDecimals={false} hide />
              <ChartTooltip
                content={
                  <ChartTooltipContent
                    labelFormatter={(_label, payload) =>
                      new Date(
                        Number(payload[0]?.payload.time)
                      ).toLocaleString()
                    }
                  />
                }
                cursor={{
                  stroke: 'var(--primary)',
                  strokeWidth: 1,
                  strokeDasharray: '2 2',
                }}
              />
            </BarChart>
          </ChartContainer>
        ) : (
          <div className="w-full h-full bg-muted/30 rounded" />
        )}
      </div>
    </div>
  )
}

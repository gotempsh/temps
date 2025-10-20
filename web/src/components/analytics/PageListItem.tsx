import { ProjectResponse } from '@/api/client'
import { getPageHourlySessionsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { useQuery } from '@tanstack/react-query'
import { Clock, ExternalLink, TrendingUp, Users } from 'lucide-react'
import { useMemo } from 'react'
import { Link } from 'react-router-dom'
import { Area, AreaChart, XAxis, YAxis } from 'recharts'

interface PageListItemProps {
  pagePath: string
  sessions: number
  avgTime: number
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

const chartConfig = {
  sessions: {
    label: 'Sessions',
    color: 'hsl(var(--primary))',
  },
} satisfies ChartConfig

export function PageListItem({
  pagePath,
  sessions,
  avgTime,
  project,
  startDate,
  endDate,
  environment,
}: PageListItemProps) {
  // Fetch hourly stats for this specific page
  const { data: hourlyData, isLoading: hourlyLoading } = useQuery({
    ...getPageHourlySessionsOptions({
      query: {
        project_id: project.id,
        bucket_interval: '24h',
        environment_id: environment,
        page_path: pagePath,
        start_time: startDate ? startDate.toISOString() : '',
        end_time: endDate ? endDate.toISOString() : '',
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = useMemo(() => {
    if (!hourlyData?.hourly_data) return []

    // Convert hourly data to chart format
    return hourlyData.hourly_data.map((hourData) => ({
      time: new Date(hourData.timestamp).toLocaleTimeString('en-US', {
        hour: '2-digit',
        minute: '2-digit',
      }),
      sessions: hourData.session_count,
    }))
  }, [hourlyData])

  return (
    <div className="group relative flex items-center gap-4 p-4 hover:bg-muted/50 transition-colors border-b last:border-b-0">
      {/* Page Info */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 mb-2">
          <Link
            to={`/projects/${project.slug}/analytics/pages?path=${encodeURIComponent(pagePath)}`}
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
        {hourlyLoading ? (
          <div className="w-full h-full bg-muted animate-pulse rounded" />
        ) : chartData.length > 0 ? (
          <ChartContainer config={chartConfig} className="h-full w-full">
            <AreaChart
              data={chartData}
              margin={{ top: 2, right: 2, bottom: 2, left: 2 }}
            >
              <defs>
                <linearGradient
                  id={`gradient-${pagePath}`}
                  x1="0"
                  y1="0"
                  x2="0"
                  y2="1"
                >
                  <stop
                    offset="5%"
                    stopColor="hsl(var(--primary))"
                    stopOpacity={0.3}
                  />
                  <stop
                    offset="95%"
                    stopColor="hsl(var(--primary))"
                    stopOpacity={0}
                  />
                </linearGradient>
              </defs>
              <Area
                dataKey="sessions"
                stroke="hsl(var(--primary))"
                fill={`url(#gradient-${pagePath})`}
                strokeWidth={1}
                dot={false}
                activeDot={false}
              />
              <XAxis hide />
              <YAxis hide />
              <ChartTooltip
                content={<ChartTooltipContent />}
                cursor={{
                  stroke: 'hsl(var(--primary))',
                  strokeWidth: 1,
                  strokeDasharray: '2 2',
                }}
              />
            </AreaChart>
          </ChartContainer>
        ) : (
          <div className="w-full h-full bg-muted/30 rounded" />
        )}
      </div>

      {/* Trend Indicator */}
      <div className="flex items-center">
        <TrendingUp className="h-4 w-4 text-emerald-600" />
      </div>
    </div>
  )
}

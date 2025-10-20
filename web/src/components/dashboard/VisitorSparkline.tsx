// import { Area, AreaChart, ResponsiveContainer, XAxis, YAxis, Tooltip } from 'recharts'
import { cn } from '@/lib/utils'
import { useMemo } from 'react'

import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { Line, LineChart, YAxis } from 'recharts'

const chartConfig = {
  hour: {
    label: 'Hour',
    color: 'var(--chart-1)',
  },
} satisfies ChartConfig

interface VisitorSparklineProps {
  data: Array<{
    hour: string
    count: number
  }>
  className?: string
  height?: number
  isHovering?: boolean
}

export function VisitorSparkline({
  data,
  className,
  height = 60,
}: VisitorSparklineProps) {
  const chartData = useMemo(() => {
    if (!data || data.length === 0) {
      // Return empty line data
      return Array.from({ length: 24 }, (_) => ({
        value: 0,
      }))
    }

    // Process the rolling 24-hour window data
    // Data comes sorted from newest to oldest, so we reverse it
    return [...data].reverse().map((item) => ({
      hour: item.hour,
      value: item.count || 0,
    }))
  }, [data])

  // Check if we have any actual data
  const hasData = data && data.length > 0 && data.some((d) => d.count > 0)

  // If no data at all, just show a flat line
  if (!hasData) {
    return (
      <div className={cn('flex items-center', className)} style={{ height }}>
        <svg width="100%" height={height} className="opacity-20">
          <line
            x1="0"
            y1={height / 2}
            x2="100%"
            y2={height / 2}
            stroke="currentColor"
            strokeWidth="1"
          />
        </svg>
      </div>
    )
  }

  // Calculate max value for proper scaling
  const maxValue = Math.max(...chartData.map((d) => d.value), 1)

  return (
    <div className={cn('w-full', className)} style={{ height }}>
      <ChartContainer config={chartConfig} className="h-full w-full">
        <LineChart
          accessibilityLayer
          data={chartData}
          margin={{
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
          }}
          height={height}
        >
          <YAxis hide domain={[0, maxValue]} />
          <ChartTooltip
            cursor={false}
            content={
              <ChartTooltipContent
                labelFormatter={(_value, payload) => {
                  if (payload && payload[0]?.payload?.hour) {
                    const dateTime = new Date(payload[0].payload.hour)
                    return dateTime.toLocaleTimeString('en-US', {
                      hour: 'numeric',
                      hour12: true,
                    })
                  }
                  return ''
                }}
              />
            }
          />
          <Line
            dataKey="value"
            type="monotone"
            stroke="var(--color-hour)"
            strokeWidth={1.5}
            dot={false}
          />
        </LineChart>
      </ChartContainer>
      {/* <ResponsiveContainer width="100%" height="100%">
				<AreaChart data={chartData} margin={{ left: 0, right: 0 }}>
					<defs>
						<linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
							<stop
								offset="5%"
								stopColor={isHovering ? "hsl(var(--chart-2))" : "hsl(var(--primary))"}
								stopOpacity={0.4}
							/>
							<stop
								offset="95%"
								stopColor={isHovering ? "hsl(var(--chart-2))" : "hsl(var(--primary))"}
								stopOpacity={0.05}
							/>
						</linearGradient>
					</defs>
					<Tooltip
						cursor={false}
						content={({ active, payload }) => {
							if (active && payload && payload[0]) {
								return (
									<div className="bg-background/95 backdrop-blur-sm border rounded-md px-2 py-1 shadow-lg">
										<div className="text-xs font-medium">
											{payload[0].value} visitors
										</div>
									</div>
								)
							}
							return null
						}}
					/>
					<Area
						dataKey="value"
						type="monotone"
						stroke={isHovering ? "hsl(var(--chart-2))" : "hsl(var(--primary))"}
						strokeWidth={1.5}
						fill={`url(#${gradientId})`}
						dot={false}
						isAnimationActive={true}
						animationDuration={300}
					/>
				</AreaChart>
			</ResponsiveContainer> */}
    </div>
  )
}

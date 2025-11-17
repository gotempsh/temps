import { getActivityGraphOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { Skeleton } from '@/components/ui/skeleton'
import { ActivityDay } from '@/api/client'
import { format, eachDayOfInterval, eachWeekOfInterval } from 'date-fns'
import { useMemo, useState } from 'react'

interface DeploymentActivityGraphProps {
  projectId: number
}

function getIntensityColor(intensity: number): string {
  const colors = [
    'bg-gray-200 dark:bg-gray-800 hover:bg-gray-300 dark:hover:bg-gray-700', // 0 - No activity (visible gray)
    'bg-emerald-100 dark:bg-emerald-950 hover:bg-emerald-200 dark:hover:bg-emerald-900', // 1
    'bg-emerald-300 dark:bg-emerald-800 hover:bg-emerald-400 dark:hover:bg-emerald-700', // 2
    'bg-emerald-500 dark:bg-emerald-600 hover:bg-emerald-600 dark:hover:bg-emerald-500', // 3
    'bg-emerald-700 dark:bg-emerald-500 hover:bg-emerald-800 dark:hover:bg-emerald-400', // 4
  ]
  return colors[Math.min(intensity, 4)] || colors[0]
}

function getTooltipText(count: number, date: Date): string {
  const formattedDate = format(date, 'MMMM do, yyyy')
  const today = new Date()
  const isToday = format(date, 'yyyy-MM-dd') === format(today, 'yyyy-MM-dd')

  const dateLabel = isToday ? `${formattedDate} (Today)` : formattedDate

  if (count === 0) {
    return `No deployments on ${dateLabel}`
  } else if (count === 1) {
    return `1 deployment on ${dateLabel}`
  } else {
    return `${count} deployments on ${dateLabel}`
  }
}

export function DeploymentActivityGraph({ projectId }: DeploymentActivityGraphProps) {
  const [tooltip, setTooltip] = useState<{
    text: string
    x: number
    y: number
  } | null>(null)

  const { data, isLoading, error } = useQuery({
    ...getActivityGraphOptions({
      query: {
        project_id: projectId,
      },
    }),
    enabled: !!projectId,
  })

  const graphData = useMemo(() => {
    if (!data) return null

    const activityMap = new Map<string, ActivityDay>()
    data.days.forEach((day) => {
      activityMap.set(day.date, day)
    })

    // Calculate weeks and structure data
    const startDate = new Date(data.start_date)
    const endDate = new Date(data.end_date)

    // Get all weeks in the range (including partial weeks at the end)
    const weeks = eachWeekOfInterval(
      { start: startDate, end: endDate },
      { weekStartsOn: 0 } // Sunday
    )

    // Ensure we include the current week even if it's incomplete
    // Check if endDate is in the last week we generated
    const lastWeek = weeks[weeks.length - 1]
    const lastWeekEnd = new Date(lastWeek.getTime() + 6 * 24 * 60 * 60 * 1000)

    // If endDate is after the last week's end, we need to add one more week
    if (endDate > lastWeekEnd) {
      const nextWeekStart = new Date(lastWeekEnd.getTime() + 24 * 60 * 60 * 1000)
      // Adjust to Sunday
      const dayOfWeek = nextWeekStart.getDay()
      if (dayOfWeek !== 0) {
        nextWeekStart.setDate(nextWeekStart.getDate() - dayOfWeek)
      }
      weeks.push(nextWeekStart)
    }

    // Create a 2D array of weeks × days (7 days per week)
    const weekData = weeks.map((weekStart) => {
      const days = eachDayOfInterval({
        start: weekStart,
        end: new Date(weekStart.getTime() + 6 * 24 * 60 * 60 * 1000),
      })

      return days.map((day) => {
        const dateStr = format(day, 'yyyy-MM-dd')
        const activity = activityMap.get(dateStr)

        // Include all days, even outside the range (will be rendered as empty)
        const isInRange = day >= startDate && day <= endDate

        return {
          date: dateStr,
          day: day,
          count: activity?.count || 0,
          level: activity?.level || 0,
          isInRange,
        }
      })
    })

    // Get months for header - track first occurrence of each month
    const months: { label: string; weekIndex: number }[] = []
    let lastMonth = ''

    weekData.forEach((week, weekIndex) => {
      if (week.length > 0 && week[0].isInRange) {
        const currentMonth = format(week[0].day, 'MMM')
        if (currentMonth !== lastMonth) {
          months.push({ label: currentMonth, weekIndex })
          lastMonth = currentMonth
        }
      }
    })

    return {
      weeks: weekData,
      months,
      total: data.days.reduce((sum, day) => sum + day.count, 0),
    }
  }, [data])

  if (isLoading) {
    return (
      <div className="space-y-3 rounded-lg border bg-card p-6">
        <Skeleton className="h-6 w-64" />
        <Skeleton className="h-32 w-full" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="space-y-3 rounded-lg border bg-card p-6">
        <h3 className="text-lg font-semibold">Deployment Activity</h3>
        <p className="text-sm text-muted-foreground">
          Unable to load deployment activity data.
        </p>
      </div>
    )
  }

  if (!graphData || graphData.total === 0) {
    return (
      <div className="space-y-3 rounded-lg border bg-card p-6">
        <h3 className="text-lg font-semibold">0 deployments in the last year</h3>
        <p className="text-sm text-muted-foreground">
          No deployment activity yet. Deploy your project to see activity here.
        </p>
      </div>
    )
  }

  const dayLabels = ['Mon', 'Wed', 'Fri']

  return (
    <div className="space-y-3 rounded-lg border bg-card p-6 w-full lg:w-1/2">
      <div className="flex items-baseline justify-between mb-4">
        <h3 className="text-lg font-semibold">
          {graphData.total} deployment{graphData.total !== 1 ? 's' : ''} in the last year
        </h3>
      </div>

      <div className="overflow-x-auto w-full">
        <div className="inline-flex flex-col min-w-full">
          {/* Graph grid */}
          <div className="flex gap-2 w-full">
            {/* Day labels - showing Mon, Wed, Fri */}
            <div className="flex flex-col gap-0.5 justify-start flex-shrink-0 text-[10px] text-muted-foreground">
              <div className="h-3 mb-1" /> {/* Spacer for month row */}
              <div className="h-3" /> {/* Sunday - empty */}
              <div className="h-3 flex items-center">Mon</div>
              <div className="h-3" /> {/* Tuesday - empty */}
              <div className="h-3 flex items-center">Wed</div>
              <div className="h-3" /> {/* Thursday - empty */}
              <div className="h-3 flex items-center">Fri</div>
              <div className="h-3" /> {/* Saturday - empty */}
            </div>

            {/* Container for month labels and activity squares */}
            <div className="flex flex-col gap-0.5">
              {/* Month labels - positioned above squares */}
              <div className="flex gap-0.5 mb-0.5 h-3 text-[10px] text-muted-foreground">
                {graphData.weeks.map((week, weekIdx) => {
                  // Check if this week starts a new month
                  const firstDayInRange = week.find((d) => d.isInRange)
                  if (!firstDayInRange) {
                    return <div key={weekIdx} className="w-3" />
                  }

                  const monthLabel = format(firstDayInRange.day, 'MMM')
                  const isFirstWeekOfMonth = graphData.months.some(
                    (m) => m.weekIndex === weekIdx
                  )

                  return (
                    <div
                      key={weekIdx}
                      className="w-3 flex items-center justify-start"
                    >
                      {isFirstWeekOfMonth && <span>{monthLabel}</span>}
                    </div>
                  )
                })}
              </div>

              {/* Activity squares - all 7 days */}
              <div className="flex gap-0.5">
                {graphData.weeks.map((week, weekIdx) => (
                  <div key={weekIdx} className="flex flex-col gap-0.5">
                    {week.map((day, dayIdx) => {
                      // If day is outside the range, show it as invisible/empty (no background)
                      if (!day.isInRange) {
                        return <div key={dayIdx} className="w-3 h-3" />
                      }

                      // Render all days in range with their intensity color (including 0 = muted)
                      const today = new Date()
                      const isToday = format(day.day, 'yyyy-MM-dd') === format(today, 'yyyy-MM-dd')

                      return (
                        <div
                          key={day.date}
                          className={`w-3 h-3 rounded-sm transition-colors cursor-pointer ${getIntensityColor(day.level)} ${isToday ? 'ring-1 ring-blue-500 dark:ring-blue-400' : ''}`}
                          onMouseEnter={(e) => {
                            const rect = e.currentTarget.getBoundingClientRect()
                            setTooltip({
                              text: getTooltipText(day.count, day.day),
                              x: rect.left + rect.width / 2,
                              y: rect.top - 8,
                            })
                          }}
                          onMouseLeave={() => setTooltip(null)}
                        />
                      )
                    })}
                  </div>
                ))}
              </div>
            </div>
          </div>

          {/* Legend - aligned bottom left */}
          <div className="flex justify-start mt-2 gap-2">
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              <span>Less</span>
              {[0, 1, 2, 3, 4].map((level) => (
                <div
                  key={level}
                  className={`w-3 h-3 rounded-sm ${getIntensityColor(level).split(' ')[0]}`}
                  title={`Level ${level}`}
                />
              ))}
              <span>More</span>
            </div>
          </div>
        </div>
      </div>

      {/* Custom Tooltip */}
      {tooltip && (
        <div
          className="fixed z-50 px-2 py-1 text-xs text-white bg-gray-900 dark:bg-gray-100 dark:text-gray-900 rounded shadow-lg pointer-events-none whitespace-nowrap"
          style={{
            left: `${tooltip.x}px`,
            top: `${tooltip.y}px`,
            transform: 'translate(-50%, -100%)',
          }}
        >
          {tooltip.text}
        </div>
      )}
    </div>
  )
}

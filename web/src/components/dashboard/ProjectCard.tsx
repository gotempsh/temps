import { ProjectResponse } from '@/api/client'
// import { getHourlyVisitorStatsOptions } from '@/api/client/@tanstack/react-query.gen'
import { getHourlyVisitsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import { AlertCircle } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { VisitorSparkline } from './VisitorSparkline'

interface ProjectCardProps {
  project: ProjectResponse
}

export function ProjectCard({ project }: ProjectCardProps) {
  // State for hover effect
  const [isHovering, setIsHovering] = useState(false)

  // Memoize dates to prevent unnecessary re-renders
  const { startDate, endDate } = useMemo(
    () => ({
      startDate: subDays(new Date(), 1),
      endDate: new Date(),
    }),
    []
  )

  const hourlyVisitorsQuery = useQuery({
    ...getHourlyVisitsOptions({
      path: {
        project_id: project.id,
      },
      query: {
        aggregation_level: 'visitors',
        start_date: startDate.toISOString(),
        end_date: endDate.toISOString(),
      },
    }),
    staleTime: 1000 * 60 * 5, // 5 minutes - prevents constant refetching
    refetchInterval: 1000 * 60, // Refetch every minute for fresh data
  })

  const totalVisitors = useMemo(() => {
    if (!hourlyVisitorsQuery.data) return 0
    return hourlyVisitorsQuery.data.reduce((acc, curr) => acc + curr.count, 0)
  }, [hourlyVisitorsQuery.data])

  return (
    <Link
      to={`/projects/${project.slug}`}
      onMouseEnter={() => setIsHovering(true)}
      onMouseLeave={() => setIsHovering(false)}
    >
      <Card className="hover:bg-muted/50 transition-colors">
        <CardContent className="p-4">
          <div className="flex items-start justify-between">
            <div className="flex items-start gap-3">
              <Avatar className="size-10">
                <AvatarImage src={`/api/projects/${project.id}/favicon`} />
                <AvatarFallback>{project.name.charAt(0)}</AvatarFallback>
              </Avatar>
              <div className="space-y-0.5">
                <div className="flex flex-col sm:flex-row sm:items-center sm:gap-2">
                  <h2 className="font-semibold leading-none">{project.name}</h2>
                  {!project.last_deployment && (
                    <Badge variant="outline" className="mt-1 w-fit sm:mt-0">
                      Not deployed
                    </Badge>
                  )}
                </div>
                {project.last_deployment && (
                  <p className="text-xs text-muted-foreground">
                    Deployed <TimeAgo date={project.last_deployment} />
                  </p>
                )}
              </div>
            </div>
          </div>

          {/* Analytics Section */}
          {hourlyVisitorsQuery.isLoading ? (
            <>
              <div className="mt-3 flex items-baseline gap-2">
                <Skeleton className="h-8 w-16" />
                <Skeleton className="h-4 w-12" />
                <span className="text-sm text-muted-foreground">
                  visitors in last 24h
                </span>
              </div>
              <div className="mt-2 h-[60px] w-full">
                <Skeleton className="h-full w-full" />
              </div>
            </>
          ) : hourlyVisitorsQuery.isError ? (
            <div className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
              <AlertCircle className="h-4 w-4" />
              <span>Unable to load analytics</span>
            </div>
          ) : (
            <>
              <div className="mt-3 flex items-baseline gap-2">
                <div className="text-2xl font-bold">{totalVisitors || 0}</div>
                {/* {hourlyVisitorsQuery.data?.total_change !== undefined && (
									<div className={cn('flex items-center gap-1', trendDisplay.className)}>
										{trendDisplay.icon}
										<span>{Math.abs(hourlyVisitorsQuery.data.total_change)}%</span>
									</div>
								)} */}
                <span className="text-sm text-muted-foreground">
                  visitors in last 24h
                </span>
              </div>

              <VisitorSparkline
                data={
                  hourlyVisitorsQuery.data?.map((e) => ({
                    hour: e.date,
                    count: e.count,
                  })) || []
                }
                className="mt-2 w-full"
                height={60}
                isHovering={isHovering}
              />
            </>
          )}
        </CardContent>
      </Card>
    </Link>
  )
}

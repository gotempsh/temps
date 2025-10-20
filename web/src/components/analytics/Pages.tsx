import { getPagePathsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { FileText, RefreshCw } from 'lucide-react'
import React from 'react'
import { PageListItem } from './PageListItem'

interface PagesProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function Pages({
  project,
  startDate,
  endDate,
  environment,
}: PagesProps) {
  const [isRefreshing, setIsRefreshing] = React.useState(false)

  // Fetch page paths
  const { data, isLoading, error, refetch } = useQuery({
    ...getPagePathsOptions({
      query: {
        project_id: project.id,
        start_date: startDate ? startDate.toISOString() : undefined,
        end_date: endDate ? endDate.toISOString() : undefined,
        environment_id: environment,
        limit: 50,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const handleRefresh = React.useCallback(async () => {
    setIsRefreshing(true)
    await refetch()
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [refetch])

  if (error) {
    return (
      <Card>
        <CardContent className="py-8">
          <div className="flex flex-col items-center justify-center text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load page paths
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Pages</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Page performance metrics'}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            {!isLoading && data && (
              <Badge variant="secondary">
                {data.page_paths?.length || 0} pages
              </Badge>
            )}
            <Button
              variant="outline"
              size="sm"
              onClick={handleRefresh}
              disabled={isLoading || isRefreshing}
              className="gap-2"
            >
              <RefreshCw
                className={`h-4 w-4 ${isLoading || isRefreshing ? 'animate-spin' : ''}`}
              />
              Refresh
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {isLoading ? (
          <div className="p-8">
            <div className="space-y-4">
              {[...Array(5)].map((_, i) => (
                <div
                  key={i}
                  className="flex items-center justify-between p-4 border rounded-lg"
                >
                  <div className="flex items-center gap-4 flex-1">
                    <div className="space-y-2 flex-1">
                      <Skeleton className="h-4 w-48" />
                      <Skeleton className="h-3 w-32" />
                    </div>
                    <Skeleton className="h-8 w-24" />
                  </div>
                </div>
              ))}
            </div>
          </div>
        ) : !data?.page_paths || data.page_paths.length === 0 ? (
          <div className="p-8">
            <div className="flex flex-col items-center justify-center text-center">
              <div className="h-12 w-12 rounded-full bg-muted flex items-center justify-center mb-4">
                <FileText className="h-6 w-6 text-muted-foreground" />
              </div>
              <p className="text-sm font-medium">No page data found</p>
              <p className="text-sm text-muted-foreground mt-1">
                Page data will appear once users visit your application
              </p>
            </div>
          </div>
        ) : (
          <div className="divide-y">
            {data.page_paths.map((pageData) => (
              <PageListItem
                key={pageData.page_path}
                pagePath={pageData.page_path}
                sessions={pageData.session_count || 0}
                avgTime={pageData.avg_time_seconds || 0}
                project={project}
                startDate={startDate}
                endDate={endDate}
                environment={environment}
              />
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

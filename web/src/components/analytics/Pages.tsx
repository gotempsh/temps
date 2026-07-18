import {
  getPagePathsOptions,
  getPagePathsSparklinesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { PagePathSparkline, ProjectResponse } from '@/api/client/types.gen'
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
import React, { useMemo } from 'react'
import {
  InsightsPanel,
  InsightsToggleButton,
  derivePagesInsights,
  useInsightsOpen,
} from './insights'
import type { AiInsightContext } from './insights'
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
  const [insightsOpen, setInsightsOpen] = useInsightsOpen()

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

  const pagePaths = data?.page_paths

  // Build comma-separated page paths for batch sparkline query
  const pagePathsCsv = useMemo(() => {
    if (!pagePaths || pagePaths.length === 0) return ''
    return pagePaths.map((p) => p.page_path).join(',')
  }, [pagePaths])

  // Fetch all sparklines in a single batch request
  const { data: sparklineData } = useQuery({
    ...getPagePathsSparklinesOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        page_paths: pagePathsCsv,
        start_time: startDate ? startDate.toISOString() : '',
        end_time: endDate ? endDate.toISOString() : '',
      },
    }),
    enabled: !!startDate && !!endDate && pagePathsCsv.length > 0,
  })

  const sparklines = sparklineData?.sparklines

  // Index sparklines by page_path for O(1) lookup
  const sparklinesByPath = useMemo(() => {
    const map = new Map<string, PagePathSparkline>()
    if (sparklines) {
      for (const sparkline of sparklines) {
        map.set(sparkline.page_path, sparkline)
      }
    }
    return map
  }, [sparklines])

  const handleRefresh = React.useCallback(async () => {
    setIsRefreshing(true)
    await refetch()
    setTimeout(() => setIsRefreshing(false), 1000)
  }, [refetch])

  const insights = useMemo(
    () => derivePagesInsights(pagePaths ?? []),
    [pagePaths]
  )

  const aiContext = useMemo<AiInsightContext | undefined>(() => {
    if (!pagePaths?.length) return undefined
    return {
      surface: 'top pages',
      rangeStart: startDate?.toISOString(),
      rangeEnd: endDate?.toISOString(),
      stats: {
        pages: pagePaths.slice(0, 10).map((p) => ({
          path: p.page_path,
          page_views: p.page_view_count,
          sessions: p.session_count,
          avg_time_seconds: p.avg_time_seconds ?? null,
        })),
      },
    }
  }, [pagePaths, startDate, endDate])

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
    <div className="flex flex-col gap-4 sm:gap-6">
      {insightsOpen && (
        <InsightsPanel
          insights={insights}
          isLoading={isLoading}
          aiContext={aiContext}
          project={{ id: project.id, slug: project.slug, name: project.name }}
        />
      )}
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
              <InsightsToggleButton
                open={insightsOpen}
                onToggle={setInsightsOpen}
              />
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
            <div className="divide-y">
              {[...Array(5)].map((_, i) => (
                <div
                  key={`page-skeleton-${i}`}
                  className="flex items-center gap-4 p-4"
                >
                  {/* Page info skeleton — matches PageListItem layout */}
                  <div className="flex-1 min-w-0">
                    <Skeleton
                      className="h-4 mb-2"
                      style={{ width: `${140 + (i % 3) * 40}px` }}
                    />
                    <div className="flex items-center gap-4">
                      <Skeleton className="h-3 w-24" />
                      <Skeleton className="h-3 w-16" />
                    </div>
                  </div>
                  {/* Mini chart skeleton */}
                  <Skeleton className="w-24 h-10 rounded" />
                  {/* Trend icon skeleton */}
                  <Skeleton className="h-4 w-4 rounded-full" />
                </div>
              ))}
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
                  sparkline={sparklinesByPath.get(pageData.page_path)}
                />
              ))}
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

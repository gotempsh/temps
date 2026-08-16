import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import { ExternalLink, FileText } from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router'

interface PagesChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function PagesChart({
  project,
  startDate,
  endDate,
  environment,
}: PagesChartProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()

  /** Build a query string that preserves the current date filter */
  function buildDateParams(extra?: Record<string, string>): string {
    const params = new URLSearchParams()
    // Forward date filter params from the overview
    const filter = searchParams.get('filter')
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    if (filter) params.set('filter', filter)
    if (from) params.set('from', from)
    if (to) params.set('to', to)
    if (extra) {
      for (const [k, v] of Object.entries(extra)) {
        params.set(k, v)
      }
    }
    const qs = params.toString()
    return qs ? `?${qs}` : ''
  }

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'pathname',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedPages = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        page: item.value || '/',
        visitors: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data])

  function handlePageClick(pagePath: string, e: React.MouseEvent) {
    const url = `/projects/${project.slug}/analytics/pages${buildDateParams({ path: pagePath })}`
    if (e.metaKey || e.ctrlKey) {
      window.open(url, '_blank')
    } else {
      navigate(url)
    }
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Top Pages</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <Button
            variant="ghost"
            size="sm"
            className="text-xs"
            onClick={() =>
              navigate(
                `/projects/${project.slug}/analytics/pages${buildDateParams()}`,
              )
            }
          >
            View all
            <ExternalLink className="ml-1 h-3 w-3" />
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div
                  key={`skeleton-page-${i}`}
                  className="flex items-center justify-between"
                >
                  <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                  <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
                </div>
              ))}
            </div>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load page analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedPages.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {sortedPages.map((page) => (
              <button
                type="button"
                key={page.page}
                className="space-y-2 w-full text-left cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1"
                onClick={(e) => handlePageClick(page.page, e)}
              >
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <FileText className="h-4 w-4 text-muted-foreground" />
                    <span className="text-sm font-medium text-muted-foreground truncate max-w-[150px] sm:max-w-[300px]">
                      {page.page}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-muted-foreground">
                      {page.percentage}%
                    </span>
                    <span className="text-sm font-mono text-muted-foreground">
                      {page.visitors.toLocaleString()}
                    </span>
                  </div>
                </div>
                <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary rounded-full transition-all duration-500"
                    style={{ width: `${page.percentage}%` }}
                  />
                </div>
              </button>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedPages.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedPages.length} pages by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

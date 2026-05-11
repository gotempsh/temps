import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { BrowserLogo } from '@/components/ui/browser-logo'
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
import { ChevronLeft } from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { buildAnalyticsDimensionUrl } from './viewAllUrl'

interface BrowsersChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function BrowsersChart({
  project,
  startDate,
  endDate,
  environment,
}: BrowsersChartProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const [selectedBrowser, setSelectedBrowser] = React.useState<string | null>(
    null
  )

  const groupBy = selectedBrowser ? 'browser_version' : 'browser'

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: groupBy,
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
        ...(selectedBrowser ? { filter_browser: selectedBrowser } : {}),
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedBrowsers = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((browser) => ({
        browser: browser.value || 'Unknown',
        count: browser.count,
        percentage: ((browser.count / total) * 100).toFixed(1),
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              {selectedBrowser && (
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-6 w-6"
                  onClick={() => setSelectedBrowser(null)}
                >
                  <ChevronLeft className="h-4 w-4" />
                </Button>
              )}
              {selectedBrowser ? `${selectedBrowser} Versions` : 'Browsers'}
            </CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            {!selectedBrowser && (
              <Badge variant="outline" className="text-xs">
                Click to drill down
              </Badge>
            )}
            {!selectedBrowser && (
              <Button
                variant="ghost"
                size="sm"
                className="text-xs"
                onClick={() =>
                  navigate(
                    buildAnalyticsDimensionUrl(
                      project.slug,
                      'browsers',
                      searchParams
                    )
                  )
                }
              >
                View all
              </Button>
            )}
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div
                  key={`skeleton-browser-${i}`}
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
              Failed to load browser analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedBrowsers.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {sortedBrowsers.map((browser) => (
              <button
                type="button"
                key={browser.browser}
                className={`space-y-2 w-full text-left ${!selectedBrowser && browser.browser !== 'Unknown' ? 'cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1' : ''}`}
                onClick={() => {
                  if (!selectedBrowser && browser.browser !== 'Unknown') {
                    setSelectedBrowser(browser.browser)
                  }
                }}
                disabled={!!selectedBrowser || browser.browser === 'Unknown'}
              >
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <BrowserLogo
                      browser={selectedBrowser || browser.browser}
                      size={20}
                    />
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">
                        {browser.browser}
                      </span>
                      {browser.browser.includes('Mobile') && (
                        <Badge
                          variant="outline"
                          className="text-xs px-1 py-0 h-4"
                        >
                          Mobile
                        </Badge>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-muted-foreground">
                      {browser.percentage}%
                    </span>
                    <span className="text-sm font-mono text-muted-foreground">
                      {browser.count.toLocaleString()}
                    </span>
                  </div>
                </div>
                <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary rounded-full transition-all duration-500"
                    style={{ width: `${browser.percentage}%` }}
                  />
                </div>
              </button>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedBrowsers.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedBrowsers.length}{' '}
            {selectedBrowser ? 'versions' : 'browsers'} by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

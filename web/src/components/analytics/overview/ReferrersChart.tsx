import { ReferrerIcon, getReferrerDisplayName } from './ReferrerIdentity'
import { AnalyticsBreakdownRow } from './AnalyticsBreakdownRow'
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getPropertyBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
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
import { ChevronLeft, Globe } from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { buildAnalyticsDimensionUrl } from './viewAllUrl'

interface ReferrersChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function ReferrersChart({
  project,
  startDate,
  endDate,
  environment,
}: ReferrersChartProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const [selectedReferrer, setSelectedReferrer] = React.useState<string | null>(
    null
  )

  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'referrer_hostname',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  // When a referrer is selected, show top pages filtered by that referrer
  const { data: pagesData } = useQuery({
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
        filter_referrer: selectedReferrer ?? undefined,
        limit: 5,
      },
    }),
    enabled: !!selectedReferrer && !!startDate && !!endDate,
  })

  const sortedReferrers = React.useMemo(() => {
    if (!data) return []
    const total = data.total
    return [...data.items]
      .sort((a, b) => b.count - a.count)
      .slice(0, 10)
      .map((referrer) => {
        const hostname = referrer.value || 'Direct'
        return {
          hostname,
          displayName: getReferrerDisplayName(hostname),
          count: referrer.count,
          percentage:
            total > 0 ? ((referrer.count / total) * 100).toFixed(1) : '0.0',
        }
      })
  }, [data])

  // Detail view for a selected referrer
  if (selectedReferrer) {
    const referrer = sortedReferrers.find(
      (r) => r.hostname === selectedReferrer
    )
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              className="h-6 w-6"
              onClick={() => setSelectedReferrer(null)}
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <ReferrerIcon domain={selectedReferrer} className="h-5 w-5" />
            {getReferrerDisplayName(selectedReferrer)}
          </CardTitle>
          <CardDescription>
            {referrer
              ? `${referrer.count.toLocaleString()} visitors (${referrer.percentage}%)`
              : ''}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="space-y-4">
            <div>
              <p className="text-sm font-medium mb-1 text-muted-foreground">
                Hostname
              </p>
              <p className="text-sm font-mono">{selectedReferrer}</p>
            </div>
            {selectedReferrer !== 'Direct' && (
              <div>
                <Badge variant="outline" className="text-xs">
                  <a
                    href={`https://${selectedReferrer}`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="flex items-center gap-1"
                  >
                    Visit site
                    <Globe className="h-3 w-3" />
                  </a>
                </Badge>
              </div>
            )}
            {pagesData && pagesData.items.length > 0 && (
              <div>
                <p className="text-sm font-medium mb-2 text-muted-foreground">
                  Top Pages
                </p>
                <div className="space-y-2">
                  {pagesData.items.slice(0, 5).map((page) => (
                    <div
                      key={page.value}
                      className="flex items-center justify-between text-sm"
                    >
                      <span className="font-mono truncate max-w-[200px]">
                        {page.value || '/'}
                      </span>
                      <span className="text-muted-foreground">
                        {page.count.toLocaleString()}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            )}
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
            <CardTitle>Referrers</CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            <Badge variant="outline" className="text-xs">
              Click for details
            </Badge>
            <Button
              variant="ghost"
              size="sm"
              className="text-xs"
              onClick={() =>
                navigate(
                  buildAnalyticsDimensionUrl(
                    project.slug,
                    'referrers',
                    searchParams
                  )
                )
              }
            >
              View all
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              {[...Array(5)].map((_, i) => (
                <div
                  key={`skeleton-ref-${i}`}
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
              Failed to load referrer analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedReferrers.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {sortedReferrers.map((referrer) => (
              <AnalyticsBreakdownRow
                key={referrer.hostname}
                label={referrer.displayName}
                icon={<ReferrerIcon domain={referrer.hostname} />}
                count={referrer.count}
                percentage={Number(referrer.percentage)}
                onClick={() => setSelectedReferrer(referrer.hostname)}
              />
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedReferrers.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedReferrers.length} referrers by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

import { AnalyticsBreakdownRow } from './AnalyticsBreakdownRow'
import { getLanguageName } from '@/lib/analytics-language'
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { Languages } from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { buildAnalyticsDimensionUrl } from './viewAllUrl'

interface LanguagesChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function LanguagesChart({
  project,
  startDate,
  endDate,
  environment,
}: LanguagesChartProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const { data, isLoading, error } = useQuery({
    ...getPropertyBreakdownOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_date: startDate ? startDate.toISOString() : '',
        end_date: endDate ? endDate.toISOString() : '',
        group_by: 'language',
        environment_id: environment,
        aggregation_level: 'visitors',
        limit: 10,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const sortedLanguages = React.useMemo(() => {
    if (!data) return []
    const total = data.items.reduce((sum, item) => sum + item.count, 0)
    return data.items
      .sort((a, b) => b.count - a.count)
      .map((item) => ({
        code: item.value || 'Unknown',
        name: getLanguageName(item.value || ''),
        count: item.count,
        percentage: ((item.count / total) * 100).toFixed(1),
      }))
  }, [data])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Languages</CardTitle>
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
                buildAnalyticsDimensionUrl(
                  project.slug,
                  'languages',
                  searchParams
                )
              )
            }
          >
            View all
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            {[...Array(5)].map((_, i) => (
              <div
                key={`skeleton-${i}`}
                className="flex items-center justify-between"
              >
                <div className="h-4 w-[150px] bg-muted animate-pulse rounded" />
                <div className="h-4 w-[100px] bg-muted animate-pulse rounded" />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load language analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !sortedLanguages.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No data available for the selected period
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {sortedLanguages.map((lang) => (
              <AnalyticsBreakdownRow
                key={lang.code}
                label={lang.name}
                icon={<Languages className="size-4 text-muted-foreground" />}
                count={lang.count}
                percentage={Number(lang.percentage)}
                subtitle={lang.code !== lang.name ? lang.code : undefined}
              />
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && sortedLanguages.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="leading-none text-muted-foreground">
            Showing top {sortedLanguages.length} languages by unique visitors
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

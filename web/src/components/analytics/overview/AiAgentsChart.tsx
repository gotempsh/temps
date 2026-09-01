// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getAiAgentBreakdownOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { AiAgentLogo } from '@/components/ui/ai-agent-logo'
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
import { ArrowRight } from 'lucide-react'
import * as React from 'react'
import { useNavigate, useSearchParams } from 'react-router'

/** How many rows the overview card shows before "View all". */
const TOP_N = 5

interface AiAgentsChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

export function AiAgentsChart({
  project,
  startDate,
  endDate,
  environment,
}: AiAgentsChartProps) {
  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const [groupBy, setGroupBy] = React.useState<'provider' | 'agent'>('provider')

  // "View all" carries the active date filter forward to the detail page so it
  // opens on the same window the user was looking at.
  const goToDetail = React.useCallback(() => {
    const params = new URLSearchParams()
    const filter = searchParams.get('filter')
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    if (filter) params.set('filter', filter)
    if (from) params.set('from', from)
    if (to) params.set('to', to)
    const qs = params.toString()
    navigate(
      `/projects/${project.slug}/analytics/ai-agents${qs ? `?${qs}` : ''}`
    )
  }, [navigate, project.slug, searchParams])

  const { data, isLoading, error } = useQuery({
    ...getAiAgentBreakdownOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
        limit: 50,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const rows = React.useMemo(() => {
    if (!data) return []
    const items = data.items ?? []
    if (items.length === 0) return []

    const total = items.reduce((sum, r) => sum + r.request_count, 0)

    if (groupBy === 'agent') {
      return items.map((row) => ({
        provider: row.provider,
        agent: row.agent,
        count: row.request_count,
        uniqueIps: row.unique_ips,
        percentage: total > 0 ? (row.request_count / total) * 100 : 0,
      }))
    }

    // Group by provider — sum agent counts.
    const byProvider = new Map<
      string,
      { count: number; uniqueIps: number; sample: string }
    >()
    for (const row of items) {
      const prev = byProvider.get(row.provider)
      if (prev) {
        prev.count += row.request_count
        prev.uniqueIps += row.unique_ips
      } else {
        byProvider.set(row.provider, {
          count: row.request_count,
          uniqueIps: row.unique_ips,
          sample: row.agent,
        })
      }
    }

    return Array.from(byProvider.entries())
      .map(([provider, v]) => ({
        provider,
        agent: v.sample,
        count: v.count,
        uniqueIps: v.uniqueIps,
        percentage: total > 0 ? (v.count / total) * 100 : 0,
      }))
      .sort((a, b) => b.count - a.count)
  }, [data, groupBy])

  const totalRequests = React.useMemo(
    () => rows.reduce((sum, r) => sum + r.count, 0),
    [rows]
  )

  const handleDrillToLogs = (provider: string, agent?: string) => {
    const params = new URLSearchParams()
    if (agent) params.set('ai_agent', agent)
    else params.set('ai_provider', provider)
    params.set('is_ai_agent', 'true')
    if (startDate) params.set('start_date', startDate.toISOString())
    if (endDate) params.set('end_date', endDate.toISOString())
    params.set('filters', 'open')
    navigate(`/projects/${project.slug}/logs?${params.toString()}`)
  }

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle className="flex items-center gap-2">
              AI Agents
              <Badge variant="secondary" className="text-xs font-normal">
                from request logs
              </Badge>
            </CardTitle>
            <CardDescription>
              {startDate && endDate
                ? `${format(startDate, 'LLL dd, y')} - ${format(endDate, 'LLL dd, y')}`
                : 'Select a date range'}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            <div className="flex items-center gap-1 rounded-md border p-0.5">
              <Button
                size="sm"
                variant={groupBy === 'provider' ? 'default' : 'ghost'}
                className="h-6 px-2 text-xs"
                onClick={() => setGroupBy('provider')}
              >
                By provider
              </Button>
              <Button
                size="sm"
                variant={groupBy === 'agent' ? 'default' : 'ghost'}
                className="h-6 px-2 text-xs"
                onClick={() => setGroupBy('agent')}
              >
                By agent
              </Button>
            </div>
            <Button
              variant="ghost"
              size="sm"
              className="text-xs"
              onClick={goToDetail}
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
                  key={`skeleton-ai-${i}`}
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
              Failed to load AI agent analytics
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !rows.length ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No AI crawlers hit your site in this period
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              We watch for OpenAI, Anthropic, Perplexity, Google, Apple, Meta,
              Amazon, ByteDance and more.
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {rows.slice(0, TOP_N).map((row) => (
              <button
                type="button"
                key={`${row.provider}-${row.agent}`}
                className="space-y-2 w-full text-left cursor-pointer hover:bg-muted/50 rounded-lg p-1 -mx-1"
                onClick={() =>
                  handleDrillToLogs(
                    row.provider,
                    groupBy === 'agent' ? row.agent : undefined
                  )
                }
              >
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-3">
                    <AiAgentLogo
                      provider={row.provider}
                      agent={row.agent}
                      size={20}
                    />
                    <div className="flex items-center gap-2">
                      <span className="text-sm font-medium">
                        {groupBy === 'agent' ? row.agent : row.provider}
                      </span>
                      {groupBy === 'agent' && (
                        <Badge
                          variant="outline"
                          className="text-xs px-1 py-0 h-4"
                        >
                          {row.provider}
                        </Badge>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-muted-foreground">
                      {row.percentage.toFixed(1)}%
                    </span>
                    <span className="text-sm font-mono text-muted-foreground">
                      {row.count.toLocaleString()}
                    </span>
                  </div>
                </div>
                <div className="relative h-2 bg-muted rounded-full overflow-hidden">
                  <div
                    className="absolute inset-y-0 left-0 bg-primary rounded-full transition-all duration-500"
                    style={{ width: `${row.percentage}%` }}
                  />
                </div>
              </button>
            ))}
          </div>
        )}
      </CardContent>
      {!isLoading && !error && rows.length > 0 && (
        <CardFooter className="flex-col items-start gap-2 text-sm">
          <div className="flex w-full items-center justify-between gap-2">
            <div className="leading-none text-muted-foreground">
              {totalRequests.toLocaleString()} AI requests across{' '}
              {rows.length.toLocaleString()}{' '}
              {groupBy === 'agent' ? 'agents' : 'providers'}
              {rows.length > TOP_N
                ? ` — showing top ${TOP_N}`
                : ''}
              .
            </div>
            {rows.length > TOP_N && (
              <Button
                variant="ghost"
                size="sm"
                className="h-7 shrink-0 gap-1 text-xs"
                onClick={goToDetail}
              >
                View all {rows.length}
                <ArrowRight className="h-3 w-3" />
              </Button>
            )}
          </div>
        </CardFooter>
      )}
    </Card>
  )
}

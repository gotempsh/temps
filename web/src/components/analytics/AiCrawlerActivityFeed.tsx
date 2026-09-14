// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProxyLogsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProxyLogResponse } from '@/api/client/types.gen'
import { AiAgentLogo } from '@/components/ui/ai-agent-logo'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { DateTimeRange } from '@/components/ui/date-time-range'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { resolveTimeRange, serializeTimeRange } from '@/lib/time-range-filter'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { AGENT_TO_PROVIDER, AI_PROVIDERS } from '@/lib/ai-agents'
import { proxyLogDetailUrl } from '@/lib/proxy-log-navigation'
import { useQuery } from '@tanstack/react-query'
import { format, formatDistanceToNow } from 'date-fns'
import { Bot, ExternalLink, RefreshCw } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router'

const ALL = '__all__'
const PAGE_SIZE_OPTIONS = [25, 50, 100] as const
const DEFAULT_PAGE_SIZE = 50

/** Color a status-code pill the way the rest of the app does. */
function statusVariant(
  status: number
): 'default' | 'secondary' | 'destructive' | 'outline' {
  if (status >= 500) return 'destructive'
  if (status >= 400) return 'outline'
  if (status >= 300) return 'secondary'
  return 'default'
}

interface AiCrawlerActivityFeedProps {
  /** Optional project scope. Omit for a global, all-projects feed. */
  projectId?: number
  /** Optional environment scope within the project. */
  environmentId?: number
}

/**
 * Chronological feed of AI crawler requests (ClaudeBot, GPTBot, PerplexityBot,
 * …) newest-first. Reads the existing `GET /proxy-logs?is_ai_agent=true`
 * endpoint, which already defaults to `timestamp DESC` and filters to the
 * known-agent taxonomy. Provider/agent/path filters are synced to the URL so
 * views are shareable.
 *
 * These rows are crawler *fetches* (AI reading your pages), not citations
 * (AI sending humans to you) — see the page copy.
 */
export function AiCrawlerActivityFeed({
  projectId,
  environmentId,
}: AiCrawlerActivityFeedProps) {
  const [searchParams, setSearchParams] = useSearchParams()

  const range = searchParams.get('range') || '24h'
  const [now, setNow] = useState(Date.now)
  // Freeze the window while paging so new requests cannot shift page boundaries.
  const timeRange = useMemo(() => resolveTimeRange(range, now), [range, now])

  const provider = searchParams.get('ai_provider') || ''
  const agent = searchParams.get('ai_agent') || ''
  const path = searchParams.get('path') || ''
  const page = Math.max(1, parseInt(searchParams.get('page') || '1', 10) || 1)
  // Configurable items-per-page, persisted in the URL. Falls back to the
  // default when the param is absent or not one of the allowed sizes.
  const parsedPageSize = parseInt(
    searchParams.get('page_size') || String(DEFAULT_PAGE_SIZE),
    10
  )
  const pageSize = (PAGE_SIZE_OPTIONS as readonly number[]).includes(
    parsedPageSize
  )
    ? parsedPageSize
    : DEFAULT_PAGE_SIZE

  // Agents available in the agent dropdown: all, or just the selected provider's.
  const agentOptions = useMemo(() => {
    if (provider) {
      return AI_PROVIDERS.find((p) => p.provider === provider)?.agents ?? []
    }
    return AI_PROVIDERS.flatMap((p) => p.agents)
  }, [provider])

  const setParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(searchParams)
    if (value) next.set(key, value)
    else next.delete(key)
    // Reset to page 1 whenever a filter changes.
    if (key !== 'page') next.delete('page')
    setSearchParams(next, { replace: true })
  }

  const { data, isLoading, error, refetch } = useQuery({
    ...getProxyLogsOptions({
      query: {
        project_id: projectId || null,
        environment_id: environmentId || null,
        is_ai_agent: true,
        start_date: timeRange.from,
        end_date: timeRange.to,
        ai_provider: provider || null,
        ai_agent: agent || null,
        path: path || null,
        sort_by: 'timestamp',
        sort_order: 'desc',
        page,
        page_size: pageSize,
      },
    }),
    staleTime: 1000 * 15,
  })

  const logs = data?.logs ?? []
  const totalPages = data?.total_pages ?? 0
  const total = data?.total ?? 0

  const hasActiveFilters = Boolean(provider || agent || path)

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <DateTimeRange
          value={timeRange}
          maxRangeDays={projectId ? 30 : 7}
          onChange={(value) => {
            setParam('range', serializeTimeRange(value))
          }}
        />
        <Button
          variant="outline"
          size="sm"
          aria-label="Refresh AI crawler activity"
          onClick={() => {
            setNow(Date.now())
            setParam('page', null)
            if (timeRange.preset === 'custom' && page === 1) void refetch()
          }}
        >
          <RefreshCw className="h-4 w-4" />
          Refresh
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        {format(new Date(timeRange.from), 'PPpp')} –{' '}
        {format(new Date(timeRange.to), 'PPpp')} (local time)
      </p>
      {/* Filters */}
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
        <Select
          value={provider || ALL}
          onValueChange={(v) => {
            const next = new URLSearchParams(searchParams)
            if (v === ALL) next.delete('ai_provider')
            else next.set('ai_provider', v)
            // Clearing/altering the provider invalidates a now-mismatched agent.
            next.delete('ai_agent')
            next.delete('page')
            setSearchParams(next, { replace: true })
          }}
        >
          <SelectTrigger className="w-full sm:w-[180px]">
            <SelectValue placeholder="All providers" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All providers</SelectItem>
            {AI_PROVIDERS.map((p) => (
              <SelectItem key={p.provider} value={p.provider}>
                {p.provider}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <Select
          value={agent || ALL}
          onValueChange={(v) => setParam('ai_agent', v === ALL ? null : v)}
        >
          <SelectTrigger className="w-full sm:w-[200px]">
            <SelectValue placeholder="All agents" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All agents</SelectItem>
            {agentOptions.map((a) => (
              <SelectItem key={a} value={a}>
                {a}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        {hasActiveFilters && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              const next = new URLSearchParams(searchParams)
              next.delete('ai_provider')
              next.delete('ai_agent')
              next.delete('path')
              next.delete('page')
              setSearchParams(next, { replace: true })
            }}
          >
            Clear filters
          </Button>
        )}

        <div className="text-sm text-muted-foreground sm:ml-auto">
          {!isLoading && !error && (
            <span>
              {total.toLocaleString()} request{total === 1 ? '' : 's'} in
              selected range
            </span>
          )}
        </div>
      </div>

      {/* Feed */}
      {error ? (
        <div className="rounded-md border border-destructive/30 bg-destructive/5 p-4 text-sm text-destructive">
          Failed to load AI crawler activity. Please try again.
        </div>
      ) : isLoading ? (
        <FeedSkeleton />
      ) : logs.length === 0 ? (
        <EmptyState hasActiveFilters={hasActiveFilters} />
      ) : (
        <div className="divide-y rounded-md border">
          {logs.map((log) => (
            <FeedRow key={log.request_id} log={log} />
          ))}
        </div>
      )}

      {/* Pagination + configurable page size (shown whenever there are rows) */}
      {!isLoading && !error && logs.length > 0 && (
        <ResponsivePagination
          page={page}
          pageSize={data?.page_size ?? pageSize}
          total={total}
          totalPages={Math.max(totalPages, 1)}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          onPageChange={(next) => setParam('page', String(next))}
          onPageSizeChange={(size) => setParam('page_size', String(size))}
        />
      )}
    </div>
  )
}

function FeedRow({ log }: { log: ProxyLogResponse }) {
  const agentName = log.bot_name ?? 'Unknown'
  const providerName = log.bot_name
    ? AGENT_TO_PROVIDER[log.bot_name]
    : undefined
  const ts = new Date(log.timestamp)

  return (
    <Link
      to={proxyLogDetailUrl({
        requestId: log.request_id,
        timestamp: log.timestamp,
        projectId: log.project_id,
      })}
      className="flex items-center gap-3 px-3 py-2.5 text-sm transition-colors hover:bg-muted/50"
    >
      <AiAgentLogo provider={providerName} agent={log.bot_name} size={22} />

      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-medium">{agentName}</span>
          {providerName && (
            <span className="text-xs text-muted-foreground">
              {providerName}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
          <span className="font-mono">{log.method}</span>
          <span className="truncate">
            {log.host}
            {log.path}
          </span>
        </div>
      </div>

      <Badge variant={statusVariant(log.status_code)} className="shrink-0">
        {log.status_code}
      </Badge>

      <span
        className="hidden shrink-0 text-xs text-muted-foreground sm:inline"
        title={format(ts, 'PPpp')}
      >
        {formatDistanceToNow(ts, { addSuffix: true })}
      </span>

      <ExternalLink className="hidden h-3.5 w-3.5 shrink-0 text-muted-foreground sm:block" />
    </Link>
  )
}

function FeedSkeleton() {
  return (
    <div className="divide-y rounded-md border">
      {Array.from({ length: 10 }).map((_, i) => (
        <div key={i} className="flex items-center gap-3 px-3 py-2.5">
          <Skeleton className="h-[22px] w-[22px] rounded-[4px]" />
          <div className="flex-1 space-y-1.5">
            <Skeleton className="h-3.5 w-32" />
            <Skeleton className="h-3 w-48" />
          </div>
          <Skeleton className="h-5 w-10 rounded-full" />
          <Skeleton className="hidden h-3 w-20 sm:block" />
        </div>
      ))}
    </div>
  )
}

function EmptyState({ hasActiveFilters }: { hasActiveFilters: boolean }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 rounded-md border border-dashed py-12 text-center">
      <Bot className="h-8 w-8 text-muted-foreground" />
      <p className="text-sm font-medium">
        No AI crawler activity in this range
      </p>
      <p className="max-w-md text-xs text-muted-foreground">
        {hasActiveFilters
          ? 'No requests match these filters in the current window. Try a wider time range or clear the filters.'
          : 'Try a wider time range to find older crawler requests. Requests logged before AI-agent detection was enabled are not reclassified retroactively.'}
      </p>
    </div>
  )
}

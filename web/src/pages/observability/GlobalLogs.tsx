// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import { LogExplorer } from '@/components/observability/LogExplorer'
import { useGlobalView } from '@/hooks/useGlobalView'
import { useQuery, useQueries } from '@tanstack/react-query'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { searchGlobalLogs } from '@/api/client/sdk.gen'
import type { GlobalLogSource, LogLevel } from '@/api/client/types.gen'
import { QueryContent } from '@/components/observability/GlobalPage'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { DateTimeRange } from '@/components/ui/date-time-range'
import { Button } from '@/components/ui/button'
import { LogQueryInput } from '@/components/observability/LogQueryInput'
import { positiveInteger } from '@/lib/global-observability'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { RefreshCw, Play, Pause } from 'lucide-react'
import { HelpPopover } from '@temps-sdk/ds'

const LEVELS: LogLevel[] = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']
export default function GlobalLogs() {
  const view = useGlobalView()
  usePageTitle('Logs')
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => setBreadcrumbs([{ label: 'Logs' }]), [setBreadcrumbs])
  const [auto, setAuto] = useState(false)
  const source: GlobalLogSource =
    view.params.get('source') === 'application'
      ? 'application'
      : view.params.get('source') === 'service'
        ? 'service'
        : 'collected'
  const level = LEVELS.find((value) => value === view.params.get('level'))
  const node = positiveInteger(view.params.get('node_id'))
  const deploy = positiveInteger(view.params.get('deploy_id'))
  const body = {
    start_time: view.from,
    end_time: view.to,
    source,
    envs: view.params.get('env') ? [view.params.get('env')!] : [],
    node_ids: node ? [node] : [],
    deploy_id: deploy,
    projects:
      source !== 'service' && view.projectId ? [String(view.projectId)] : [],
    levels: level ? [level] : [],
    text: view.search || undefined,
    cursor: view.params.get('cursor') || undefined,
    page_size: 100,
  }
  const query = useQuery({
    queryKey: ['global-log-search', body],
    queryFn: async ({ signal }) =>
      (await searchGlobalLogs({ body, signal, throwOnError: true })).data,
    retry: false,
  })
  const refresh = () => {
    if (view.range === 'custom') void query.refetch()
    else view.setRange(view.range)
  }
  const refreshRef = useRef(() => {})
  useEffect(() => {
    refreshRef.current = () => {
      if (!document.hidden && !query.isFetching && !query.error) refresh()
    }
  })
  useEffect(() => {
    if (!auto) return
    const timer = window.setInterval(() => refreshRef.current(), 5000)
    return () => window.clearInterval(timer)
  }, [auto])
  const filter = (patch: Record<string, string | undefined>) => {
    setAuto(false)
    view.patch(patch)
  }
  const ready = !query.error && !query.isPending
  const incomplete = ready && !!query.data?.scan_limit_reached
  const lines = ready ? (query.data?.lines ?? []) : []
  const environmentProjects = [
    ...new Set([
      ...(view.projectId ? [view.projectId] : []),
      ...lines.flatMap((line) =>
        line.project_id != null ? [line.project_id] : []
      ),
    ]),
  ]
  const environmentQueries = useQueries({
    queries: environmentProjects.map((project_id) => ({
      ...getEnvironmentsOptions({ path: { project_id } }),
      staleTime: 60_000,
    })),
  })
  const environmentLabels = Object.fromEntries(
    environmentQueries.flatMap((query) =>
      (query.data ?? []).map((environment) => [
        String(environment.id),
        environment.slug,
      ])
    )
  )
  const status =
    incomplete && !lines.length ? (
      <p className="py-6 text-sm text-muted-foreground">
        No matching lines in this scan. Continue to the next page or narrow your
        search.
      </p>
    ) : !ready || !lines.length ? (
      <QueryContent
        title="Logs"
        loading={query.isPending}
        error={query.error}
        empty={!lines.length}
        retry={() => void query.refetch()}
      >
        {null}
      </QueryContent>
    ) : undefined
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Logs"
        description={`${view.projectId ? 'Selected project' : 'All projects'} · application and database logs`}
      />
      <LogExplorer
        lines={lines}
        environmentLabels={environmentLabels}
        onFilter={filter}
        onInspect={() => setAuto(false)}
        status={status}
        toolbar={
          <div role="region" aria-label="Logs filters" className="space-y-2">
            <LogQueryInput
              params={view.params}
              text={view.search}
              lines={lines}
              environmentLabels={environmentLabels}
              onChange={filter}
            />
            <div className="flex flex-wrap items-center gap-2">
              <DateTimeRange
                value={{ from: view.from, to: view.to, preset: view.range }}
                onChange={(range) => {
                  setAuto(false)
                  view.setTimeRange(range)
                }}
              />
              <span className="text-[11px] text-muted-foreground">UTC</span>
              <HelpPopover label="About log search">
                <p>
                  Search messages or use project:, env:, source:, and level:
                  filters.
                </p>
                <p>
                  Counts and groups describe the loaded page. Times are in UTC.
                </p>
              </HelpPopover>
              <Button
                size="sm"
                variant="ghost"
                className="h-6 gap-1.5 px-1 text-xs"
                aria-pressed={auto && !query.error}
                disabled={!!query.error}
                onClick={() => {
                  if (!auto && view.params.has('cursor')) view.setCursor()
                  setAuto(!auto)
                }}
              >
                {auto && !query.error ? (
                  <Pause className="size-3" />
                ) : (
                  <Play className="size-3" />
                )}
                {auto && !query.error ? 'Auto-refresh · 5s' : 'Paused'}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-6 gap-1.5 px-1 text-xs"
                disabled={query.isFetching}
                onClick={refresh}
              >
                <RefreshCw
                  className={`size-3 ${query.isFetching ? 'animate-spin' : ''}`}
                />
                Refresh
              </Button>
              {([
                'level',
                'project_id',
                'source',
                'env',
                'node_id',
                'deploy_id',
              ].some((key) => view.params.has(key)) ||
                view.search) && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 px-1 text-xs underline"
                  onClick={() =>
                    filter({
                      q: undefined,
                      level: undefined,
                      project_id: undefined,
                      source: undefined,
                      env: undefined,
                      node_id: undefined,
                      deploy_id: undefined,
                    })
                  }
                >
                  Clear filters
                </Button>
              )}
            </div>
          </div>
        }
        footer={
          <div className="flex flex-wrap items-center justify-between gap-2 py-3 text-xs text-muted-foreground">
            <div className="flex items-center gap-1">
              <span>
                {ready
                  ? `${lines.length} loaded ${lines.length === 1 ? 'line' : 'lines'} · ${incomplete ? 'partial results' : 'newest first'}`
                  : query.isPending
                    ? 'Loading logs…'
                    : 'Search incomplete'}
              </span>
              {incomplete && (
                <HelpPopover label="About partial results">
                  This scan reached its limit. Use Next page to continue, or
                  narrow your search or time range.
                </HelpPopover>
              )}
            </div>
            <div className="flex gap-1">
              <Button
                variant="ghost"
                size="sm"
                className="h-7 text-xs"
                disabled={!view.params.has('cursor') || query.isFetching}
                onClick={() => {
                  setAuto(false)
                  view.setCursor()
                }}
              >
                First page
              </Button>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                disabled={
                  !query.data?.next_cursor || query.isFetching || !ready
                }
                onClick={() => {
                  setAuto(false)
                  view.setCursor(query.data?.next_cursor ?? undefined)
                }}
              >
                Next page
              </Button>
            </div>
          </div>
        }
      />
    </PageContainer>
  )
}

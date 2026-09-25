// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router'
import { LogExplorer } from '@/components/observability/LogExplorer'
import { LogHistogram } from '@/components/observability/LogHistogram'
import { LogCollectionNotice } from '@/components/observability/LogCollectionNotice'
import {
  AttrPredicateChips,
  LogAttributeSidebar,
} from '@/components/observability/LogAttributeSidebar'
import { useGlobalView } from '@/hooks/useGlobalView'
import { useQueries } from '@tanstack/react-query'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { getProject } from '@/api/client/sdk.gen'
import type { LogLevel, LogSourceKind } from '@/api/client/types.gen'
import { QueryContent } from '@/components/observability/GlobalPage'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { DateTimeRange } from '@/components/ui/date-time-range'
import { Button } from '@/components/ui/button'
import { Alert, AlertTitle, AlertDescription } from '@/components/ui/alert'
import { LogQueryInput } from '@/components/observability/LogQueryInput'
import { positiveInteger } from '@/lib/global-observability'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  EXPLORER_FACET_FIELDS,
  GLOBAL_LOG_FOLLOW_INTERVAL_MS,
  facetValues,
  useGlobalLogCapabilities,
  useGlobalLogFacets,
  useGlobalLogSearch,
  type GlobalLogFilters,
} from '@/hooks/useGlobalLogs'
import { RefreshCw, Play, Pause, Loader2 } from 'lucide-react'
import { Callout, HelpPopover } from '@temps-sdk/ds'

const LEVELS: LogLevel[] = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']

export default function GlobalLogs() {
  const view = useGlobalView()
  usePageTitle('Logs')
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => setBreadcrumbs([{ label: 'Logs' }]), [setBreadcrumbs])
  const [follow, setFollow] = useState(false)
  const source: LogSourceKind =
    view.params.get('source') === 'application'
      ? 'application'
      : view.params.get('source') === 'service'
        ? 'service'
        : 'collected'
  const level = LEVELS.find((value) => value === view.params.get('level'))
  const node = positiveInteger(view.params.get('node_id'))
  const deploy = positiveInteger(view.params.get('deploy_id'))
  const env = view.params.get('env')
  const projectId = view.projectId
  const text = view.search
  // Attribute predicates (ADR-047 §5) are stored as repeated `attr` params,
  // separately from `view.patch`'s single-value filters — `useSearchParams`
  // shares the same router state `useGlobalView` reads, so both stay in sync.
  const [searchParams, setSearchParams] = useSearchParams()
  // Keyed on the serialized params (a stable primitive) rather than
  // `searchParams` itself, so this array keeps its identity across renders
  // that don't change the query — otherwise every render would hand
  // `filters` a new `attrs` array and re-trigger every query that reads it.
  const searchParamsKey = searchParams.toString()
  const attrs = useMemo(
    () => searchParams.getAll('attr'),
    [searchParamsKey] // eslint-disable-line react-hooks/exhaustive-deps
  )
  const addAttr = (predicate: string) =>
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete('page')
        next.delete('cursor')
        if (!next.getAll('attr').includes(predicate))
          next.append('attr', predicate)
        return next
      },
      { replace: true }
    )
  const removeAttr = (predicate: string) =>
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.delete('page')
        next.delete('cursor')
        const kept = next.getAll('attr').filter((p) => p !== predicate)
        next.delete('attr')
        for (const p of kept) next.append('attr', p)
        return next
      },
      { replace: true }
    )
  const filters: GlobalLogFilters = useMemo(
    () => ({
      start_time: view.from,
      end_time: view.to,
      source,
      envs: env ? [env] : [],
      node_ids: node ? [node] : [],
      deploy_id: deploy,
      projects: source !== 'service' && projectId ? [String(projectId)] : [],
      levels: level ? [level] : [],
      text: text || undefined,
      attrs: attrs.length ? attrs : undefined,
    }),
    [
      view.from,
      view.to,
      source,
      env,
      node,
      deploy,
      projectId,
      level,
      text,
      attrs,
    ]
  )

  const search = useGlobalLogSearch(filters, follow)
  const facets = useGlobalLogFacets(filters, EXPLORER_FACET_FIELDS)
  const capabilities = useGlobalLogCapabilities()

  // A historical window can't grow, so following it would poll forever for
  // lines that cannot arrive. Say so rather than offering a dead toggle.
  const followable = view.range !== 'custom'
  const following = follow && followable && !search.error

  const refresh = () => {
    if (view.range === 'custom') search.refresh()
    else view.setRange(view.range)
  }
  const filter = (patch: Record<string, string | undefined>) => {
    setFollow(false)
    view.patch(patch)
  }
  const lines = search.error ? [] : search.lines

  // Facets count the whole time range, so they can name projects that have
  // no line on the loaded page. Resolve those names directly instead of
  // leaving them to the page's lines. A project this instance no longer has
  // (deleted, or lines collected before it existed) answers 404 and is shown
  // as unknown rather than under a name-shaped placeholder.
  const facetProjectIds = [
    ...new Set(
      facetValues(facets.data, 'project_id').flatMap((item) => {
        const id = positiveInteger(item.value)
        return id ? [id] : []
      })
    ),
  ]
  const projectQueries = useQueries({
    queries: facetProjectIds.map((id) => ({
      // Own key: this query resolves a missing project to `null`, which must
      // not leak into the shared project cache other pages read.
      queryKey: ['log-facet-project', id],
      // The Problem body carries no status, so a missing project is told
      // apart from a failed request by the HTTP status itself.
      queryFn: async ({ signal }: { signal: AbortSignal }) => {
        const result = await getProject({ path: { id }, signal })
        if (result.response?.status === 404) return null
        if (!result.data)
          throw new Error(
            (result.error as { detail?: string } | undefined)?.detail ??
              `Project ${id} could not be loaded.`
          )
        return result.data
      },
      staleTime: 60_000,
    })),
  })
  const projectLabels = Object.fromEntries(
    projectQueries.flatMap((query, index) => {
      const id = facetProjectIds[index]
      if (query.data) return [[String(id), query.data.name]]
      if (query.data === null) return [[String(id), `Unknown project #${id}`]]
      return []
    })
  )

  // Environment IDs are all the store keeps; the interface presents slugs.
  // Resolved per-project rather than globally, since two projects can reuse
  // the same numeric environment id for unrelated environments. Projects named
  // only by the facets count too, or their environments stay unresolved.
  const environmentProjects = [
    ...new Set([
      ...(view.projectId ? [view.projectId] : []),
      ...lines.flatMap((line) =>
        line.project_id != null ? [line.project_id] : []
      ),
      ...facetProjectIds.filter((_, index) => projectQueries[index]?.data),
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
    search.isPending || search.error || !lines.length ? (
      <QueryContent
        title="Logs"
        loading={search.isPending}
        error={search.error}
        empty={!lines.length}
        retry={search.retry}
      >
        {null}
      </QueryContent>
    ) : search.partial ? (
      <Callout tone="warning" title="Search paused">
        Searched back to{' '}
        {search.scannedBackTo
          ? new Date(search.scannedBackTo).toLocaleString()
          : 'the query budget'}{' '}
        so far — nothing older has been checked yet.{' '}
        <Button
          variant="link"
          size="sm"
          className="h-auto p-0 text-xs underline"
          disabled={search.isLoadingMore}
          onClick={search.loadMore}
        >
          {search.isLoadingMore ? 'Searching…' : 'Keep searching'}
        </Button>
      </Callout>
    ) : undefined
  return (
    <PageContainer innerClassName="space-y-6">
      <PageHeader
        title="Logs"
        description={`${view.projectId ? 'Selected project' : 'All projects'} · application and database logs`}
      />
      <LogCollectionNotice
        collection={capabilities.data?.collection}
        statusError={capabilities.error}
        statusUpdatedAt={capabilities.dataUpdatedAt || undefined}
        onRetry={() => void capabilities.refetch()}
      />
      <LogExplorer
        lines={lines}
        environmentLabels={environmentLabels}
        projectLabels={projectLabels}
        facets={facets.data}
        facetsLoading={facets.isPending}
        facetsError={facets.error}
        onRetryFacets={() => void facets.refetch()}
        onFilter={filter}
        onInspect={() => setFollow(false)}
        onLoadMore={search.loadMore}
        autoLoadMore={!search.partial}
        hasMore={!!search.hasMore}
        isLoadingMore={search.isLoadingMore}
        status={status}
        histogram={
          <LogHistogram
            filters={filters}
            capability={capabilities.data?.analytics}
            capabilityLoading={capabilities.isPending}
            onRangeSelect={(from, to) => {
              setFollow(false)
              view.setTimeRange({ from, to, preset: 'custom' })
            }}
          />
        }
        attributesPanel={
          <LogAttributeSidebar
            filters={filters}
            capability={capabilities.data?.analytics}
            capabilityLoading={capabilities.isPending}
            activePredicates={attrs}
            onAddPredicate={addAttr}
          />
        }
        toolbar={
          <div role="region" aria-label="Logs filters" className="space-y-2">
            <LogQueryInput
              params={view.params}
              text={view.search}
              environmentLabels={environmentLabels}
              filters={filters}
              onChange={filter}
            />
            {attrs.length > 0 && (
              <AttrPredicateChips predicates={attrs} onRemove={removeAttr} />
            )}
            <div className="flex flex-wrap items-center gap-2">
              <DateTimeRange
                value={{ from: view.from, to: view.to, preset: view.range }}
                onChange={(range) => {
                  setFollow(false)
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
                aria-pressed={following}
                disabled={!!search.error || !followable}
                title={
                  followable
                    ? undefined
                    : 'Following needs a live time range — pick a preset such as 1h.'
                }
                onClick={() => setFollow(!follow)}
              >
                {following ? (
                  <Pause className="size-3" />
                ) : (
                  <Play className="size-3" />
                )}
                {following
                  ? `Following · ${GLOBAL_LOG_FOLLOW_INTERVAL_MS / 1000}s`
                  : 'Paused'}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-6 gap-1.5 px-1 text-xs"
                disabled={search.isFetching}
                onClick={refresh}
              >
                <RefreshCw
                  className={`size-3 ${search.isFetching ? 'animate-spin' : ''}`}
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
                view.search ||
                attrs.length > 0) && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 px-1 text-xs underline"
                  onClick={() => {
                    filter({
                      q: undefined,
                      level: undefined,
                      project_id: undefined,
                      source: undefined,
                      env: undefined,
                      node_id: undefined,
                      deploy_id: undefined,
                    })
                    setSearchParams(
                      (current) => {
                        const next = new URLSearchParams(current)
                        next.delete('attr')
                        return next
                      },
                      { replace: true }
                    )
                  }}
                >
                  Clear filters
                </Button>
              )}
            </div>
            {following && search.followError && (
              <Alert variant="warning">
                <AlertTitle>Following stopped updating</AlertTitle>
                <AlertDescription>
                  {search.followError.message} New lines are not being added.
                  The next poll retries automatically; pause and refresh if it
                  keeps failing.
                </AlertDescription>
              </Alert>
            )}
          </div>
        }
        footer={
          <div className="flex flex-wrap items-center justify-between gap-2 py-3 text-xs text-muted-foreground">
            <span>
              {search.isPending
                ? 'Loading logs…'
                : search.error
                  ? 'Logs could not be loaded'
                  : `${lines.length} loaded ${lines.length === 1 ? 'line' : 'lines'} · newest first${
                      following && search.followedCount
                        ? ` · ${search.followedCount} new while following`
                        : ''
                    }`}
            </span>
            {!search.error && !search.isPending && (
              <div className="flex items-center gap-2">
                {search.hasMore ? (
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7 text-xs"
                    disabled={search.isLoadingMore}
                    onClick={search.loadMore}
                  >
                    {search.isLoadingMore && (
                      <Loader2 className="mr-1.5 size-3 animate-spin" />
                    )}
                    Load older lines
                  </Button>
                ) : (
                  lines.length > 0 && <span>End of results for this range</span>
                )}
              </div>
            )}
          </div>
        }
      />
    </PageContainer>
  )
}

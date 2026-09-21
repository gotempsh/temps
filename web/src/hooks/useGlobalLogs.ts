// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import {
  facetGlobalLogs,
  globalLogAttributeKeys,
  globalLogCapabilities,
  globalLogFacetsAttrs,
  globalLogHistogram,
  searchGlobalLogs,
} from '@/api/client/sdk.gen'
import type {
  AttributeKeysResponse,
  FacetField,
  FacetsAttrsResponse,
  FacetValue,
  GlobalLogFacetsResponse,
  GlobalLogLine,
  GlobalLogSearchRequest,
  HistogramResponse,
} from '@/api/client/types.gen'
import { logLineKey } from '@/lib/log-explorer'

/**
 * The filter body shared by `/logs/global/search` and `/logs/global/facets` —
 * everything except how the page is sliced. Both endpoints take the identical
 * filter set, so a facet count always describes the search on screen.
 */
export type GlobalLogFilters = Omit<
  GlobalLogSearchRequest,
  'cursor' | 'page_size'
>

/**
 * We ask for the server default and deliberately expose no page-size control.
 * With a keyset cursor and infinite scroll the page size is an implementation
 * detail — "how many lines arrive per network round trip" — not a user-facing
 * choice, and a user-settable value only invites hitting the 1,000 server cap.
 */
export const GLOBAL_LOG_PAGE_SIZE = 200

/**
 * Follow-mode poll interval. Each tick is one bounded keyset query anchored at
 * the newest line already on screen, not a re-run of the whole first page.
 */
export const GLOBAL_LOG_FOLLOW_INTERVAL_MS = 5000

const searchKey = (filters: GlobalLogFilters) =>
  ['global-log-search', filters] as const

const toError = (error: unknown) =>
  error instanceof Error ? error : new Error(String(error))

/**
 * Keyset-paginated global log search.
 *
 * `next_cursor` is a real keyset position over the store's sort order, so
 * "load older" costs the same on page 40 as on page 1 and `null` genuinely
 * means there is nothing older. A page can still be `partial`, though: the
 * store's own time/byte budget ran out before it could prove the page
 * complete. `next_cursor` stays populated on a partial page — Load
 * older/Keep searching just walks the same cursor further, appending results
 * exactly like a normal "load older" page would.
 *
 * Follow mode re-asks from the newest line currently displayed (inclusive, so
 * same-timestamp siblings are caught) up to now, keeps only identities that
 * aren't rendered yet, and prepends them.
 */
export function useGlobalLogSearch(filters: GlobalLogFilters, follow: boolean) {
  const queryClient = useQueryClient()
  const query = useInfiniteQuery({
    queryKey: searchKey(filters),
    queryFn: async ({ pageParam, signal }) =>
      (
        await searchGlobalLogs({
          body: {
            ...filters,
            cursor: pageParam,
            page_size: GLOBAL_LOG_PAGE_SIZE,
          },
          signal,
          throwOnError: true,
        })
      ).data,
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (lastPage, _pages, lastPageParam) => {
      const next = lastPage.next_cursor ?? undefined
      // Infinite scroll auto-loads whenever the rope fits the viewport, so a
      // cursor that doesn't advance would fetch forever. Treat it as the end.
      return next && next !== lastPageParam ? next : undefined
    },
    retry: false,
  })

  const paged = useMemo(
    () => (query.data?.pages ?? []).flatMap((page) => page.lines),
    [query.data]
  )

  // The most recently fetched page's own partial state — not "any page so
  // far", since an earlier partial page gets made whole the moment its
  // cursor is walked further; only the *last* fetch tells the caller whether
  // there is unsearched territory immediately behind what's on screen.
  const pages = query.data?.pages
  const lastPage =
    pages && pages.length > 0 ? pages[pages.length - 1] : undefined
  const partial = lastPage?.partial ?? false
  const scannedBackTo = partial ? (lastPage?.scanned_back_to ?? null) : null

  // Lines picked up by follow mode, tagged with the scope they were polled for:
  // any filter change starts a different search, and lines from the previous
  // one must not leak into it. Tagging (rather than clearing on change) keeps
  // the reset a pure derivation instead of a render-phase side effect.
  const scope = JSON.stringify(filters)
  const [followed, setFollowed] = useState<{
    scope: string
    lines: GlobalLogLine[]
    error?: Error
  }>({ scope, lines: [] })
  const current = followed.scope === scope ? followed : undefined
  const tail = current?.lines ?? []

  const lines = useMemo(() => [...tail, ...paged], [tail, paged])
  const linesRef = useRef(lines)
  const filtersRef = useRef(filters)
  const scopeRef = useRef(scope)
  useEffect(() => {
    linesRef.current = lines
    filtersRef.current = filters
    scopeRef.current = scope
  })

  const poll = useCallback(async () => {
    const anchor = linesRef.current[0]
    if (!anchor || document.hidden) return
    const polledScope = scopeRef.current
    try {
      const { data } = await searchGlobalLogs({
        body: {
          ...filtersRef.current,
          start_time: anchor.timestamp,
          end_time: new Date().toISOString(),
          page_size: GLOBAL_LOG_PAGE_SIZE,
        },
        throwOnError: true,
      })
      const seen = new Set(linesRef.current.map(logLineKey))
      const fresh = data.lines.filter((line) => !seen.has(logLineKey(line)))
      setFollowed((previous) => {
        const kept = previous.scope === polledScope ? previous.lines : []
        return { scope: polledScope, lines: [...fresh, ...kept] }
      })
    } catch (error) {
      // Silent failure is the one thing follow mode must never do: a visibly
      // broken tail is better than a tail that quietly stopped.
      setFollowed((previous) => ({
        scope: polledScope,
        lines: previous.scope === polledScope ? previous.lines : [],
        error: toError(error),
      }))
    }
  }, [])

  useEffect(() => {
    if (!follow) return
    const timer = window.setInterval(
      () => void poll(),
      GLOBAL_LOG_FOLLOW_INTERVAL_MS
    )
    return () => window.clearInterval(timer)
  }, [follow, poll, scope])

  const loadMore = useCallback(() => {
    if (query.hasNextPage && !query.isFetchingNextPage)
      void query.fetchNextPage()
  }, [query])

  const refresh = useCallback(() => {
    setFollowed({ scope: scopeRef.current, lines: [] })
    void queryClient.resetQueries({ queryKey: searchKey(filtersRef.current) })
  }, [queryClient])

  return {
    lines,
    error: query.error as Error | null,
    isPending: query.isPending,
    isFetching: query.isFetching,
    hasMore: query.hasNextPage,
    isLoadingMore: query.isFetchingNextPage,
    /** `true` when the last fetched page's budget ran out before it could be
     *  proven complete. `hasMore` (from `next_cursor`) already stays `true`
     *  in this case, so "Load older"/"Keep searching" is the same action. */
    partial,
    /** Set when `partial` is `true`: everything back to this point in time
     *  has been searched, nothing older has yet. */
    scannedBackTo,
    followError: follow ? current?.error : undefined,
    followedCount: tail.length,
    loadMore,
    refresh,
    retry: () => void query.refetch(),
  }
}

/** Facet fields the explorer sidebar can turn into a URL filter. */
export const EXPLORER_FACET_FIELDS: FacetField[] = [
  'level',
  'project',
  'external_service',
  'env',
  'node',
  'deploy',
]

/**
 * Distinct values + counts for the current filter scope, straight from the
 * store. Replaces counting whatever happened to be on the loaded page: a user
 * can now discover a value they have never seen on screen.
 */
export function useGlobalLogFacets(
  filters: GlobalLogFilters,
  fields: FacetField[],
  enabled = true
) {
  return useQuery({
    queryKey: ['global-log-facets', filters, fields],
    queryFn: async ({ signal }) =>
      (
        await facetGlobalLogs({
          body: { ...filters, fields },
          signal,
          throwOnError: true,
        })
      ).data,
    enabled,
    retry: false,
    staleTime: 30_000,
  })
}

/**
 * Narrow one field out of the facet map.
 *
 * The generated response types the map as `Record<string, unknown>` (utoipa
 * can't express the per-key value type), so the cast lives here once instead of
 * at every call site.
 */
export function facetValues(
  response: GlobalLogFacetsResponse | undefined,
  key: string
): FacetValue[] {
  const values = response?.facets?.[key]
  return Array.isArray(values) ? (values as FacetValue[]) : []
}

// ---------------------------------------------------------------------------
// ADR-047 attribute analytics: capabilities, histogram, attribute keys/facets
// ---------------------------------------------------------------------------

/**
 * The scope fields of `GlobalLogFilters` that the analytics GET endpoints
 * (`attributes`, `facets/attrs`, `histogram`, `aggregate`) also accept —
 * everything `GlobalLogFilterQuery` on the server merges into the same scoped
 * `LogQuery` the plain search/facets endpoints build (see
 * `handlers/global.rs`), so a histogram or attribute list always describes
 * the same source/level/env/etc. scope the line list and facet sidebar do.
 *
 * The four GET endpoints declare exactly these fields in their OpenAPI
 * params, so the generated `…Data['query']` types accept this shape as-is.
 */
type AnalyticsScopeQuery = Omit<
  Pick<
    GlobalLogSearchRequest,
    | 'start_time'
    | 'end_time'
    | 'source'
    | 'projects'
    | 'external_services'
    | 'scopes'
    | 'levels'
    | 'envs'
    | 'services'
    | 'container_ids'
    | 'node_ids'
    | 'deploy_id'
  >,
  'deploy_id'
> & { deploy_id?: number }

function analyticsScopeQuery(filters: GlobalLogFilters): AnalyticsScopeQuery {
  return {
    start_time: filters.start_time,
    end_time: filters.end_time,
    source: filters.source,
    projects: filters.projects,
    external_services: filters.external_services,
    scopes: filters.scopes,
    levels: filters.levels,
    envs: filters.envs,
    services: filters.services,
    container_ids: filters.container_ids,
    node_ids: filters.node_ids,
    deploy_id: filters.deploy_id ?? undefined,
  }
}

/** Attribute predicates as repeated `attr` query params, for the GET analytics endpoints. */
const attrParams = (filters: GlobalLogFilters): string[] => filters.attrs ?? []

/**
 * Instance capabilities for the log explorer (ADR-047 §5): whether the
 * ClickHouse line index is configured, and — either way — what to tell the
 * user (reason/example/setup path when it isn't, index coverage when it is).
 * Polled occasionally rather than once: the reindexer's coverage gap and a
 * freshly-configured instance should both become visible without a reload.
 */
export function useGlobalLogCapabilities() {
  return useQuery({
    queryKey: ['global-log-capabilities'],
    queryFn: async ({ signal }) =>
      (await globalLogCapabilities({ signal, throwOnError: true })).data,
    staleTime: 30_000,
    refetchInterval: 60_000,
    retry: false,
  })
}

/** Attribute keys observed in the current scope, with line counts (facet sidebar). */
export function useGlobalLogAttributeKeys(
  filters: GlobalLogFilters,
  enabled = true
) {
  return useQuery({
    queryKey: ['global-log-attribute-keys', filters],
    queryFn: async ({ signal }) =>
      (
        await globalLogAttributeKeys({
          query: {
            ...analyticsScopeQuery(filters),
            limit: 200,
          },
          signal,
          throwOnError: true,
        })
      ).data as AttributeKeysResponse,
    enabled,
    retry: false,
    staleTime: 30_000,
  })
}

/** Top values (+ counts) of a single attribute key, fetched when its facet row expands. */
export function useGlobalLogAttrValues(
  filters: GlobalLogFilters,
  key: string,
  enabled: boolean
) {
  return useQuery({
    queryKey: ['global-log-attr-values', filters, key],
    queryFn: async ({ signal }) =>
      (
        await globalLogFacetsAttrs({
          query: {
            ...analyticsScopeQuery(filters),
            keys: `attr:${key}`,
            attr: attrParams(filters),
            limit: 50,
          },
          signal,
          throwOnError: true,
        })
      ).data as FacetsAttrsResponse,
    enabled,
    retry: false,
    staleTime: 30_000,
  })
}

/** One attribute key's top values out of a `FacetsAttrsResponse`. */
export function attrFacetValues(
  response: FacetsAttrsResponse | undefined,
  key: string
): FacetValue[] {
  const values = response?.facets?.[key]
  return Array.isArray(values) ? values : []
}

export type HistogramGroupBy = '' | 'level' | 'service' | `attr:${string}`

/**
 * Line-count histogram for the current filters (ADR-047 §5). The index holds
 * no message bytes, so `text` is never sent here — callers that want the
 * "text filter not applied" notice compare `filters.text` themselves.
 */
export function useGlobalLogHistogram(
  filters: GlobalLogFilters,
  bucketSecs: number,
  groupBy: HistogramGroupBy,
  enabled = true
) {
  return useQuery({
    queryKey: [
      'global-log-histogram',
      analyticsScopeQuery(filters),
      attrParams(filters),
      bucketSecs,
      groupBy,
    ],
    queryFn: async ({ signal }) =>
      (
        await globalLogHistogram({
          query: {
            ...analyticsScopeQuery(filters),
            bucket_secs: bucketSecs,
            group_by: groupBy || undefined,
            max_groups: 8,
            attr: attrParams(filters),
          },
          signal,
          throwOnError: true,
        })
      ).data as HistogramResponse,
    enabled,
    retry: false,
    staleTime: 15_000,
  })
}

/**
 * Bucket width (seconds) that keeps a window's histogram in the 60–120 bucket
 * range, snapped to human-friendly widths.
 */
export function histogramBucketSeconds(startIso: string, endIso: string) {
  const spanMs = Math.max(
    1000,
    Date.parse(endIso) - Date.parse(startIso) || 60_000
  )
  const target = spanMs / 90 / 1000
  const steps = [
    1, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14400, 21600,
    43200, 86400, 172800, 604800,
  ]
  return steps.find((step) => step >= target) ?? steps[steps.length - 1]
}

/** Attribute predicate helpers (ADR-047 §5: `<key><op><value>` or `<key>?`). */
export type AttrOp = '=' | '!=' | '^=' | '>' | '<'

export function formatAttrPredicate(
  key: string,
  op: AttrOp,
  value: string
): string {
  return `${key}${op}${value}`
}

export function formatAttrExists(key: string): string {
  return `${key}?`
}

/** Parse a stored `attr` predicate back into its parts, for rendering a chip. */
export function parseAttrPredicate(
  raw: string
): { key: string; op: AttrOp | '?'; value?: string } | undefined {
  if (raw.endsWith('?')) {
    const key = raw.slice(0, -1)
    return key ? { key, op: '?' } : undefined
  }
  for (const op of ['!=', '^=', '=', '>', '<'] as const) {
    const idx = raw.indexOf(op)
    if (idx > 0) {
      return { key: raw.slice(0, idx), op, value: raw.slice(idx + op.length) }
    }
  }
  return undefined
}

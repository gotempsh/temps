import { client } from '@/api/client/client.gen'
import {
  keepPreviousData,
  useInfiniteQuery,
  useQuery,
} from '@tanstack/react-query'

// ── Types matching the Rust SearchLogsResponse ─────────────────────────

export type LogLevel = 'ERROR' | 'WARN' | 'INFO' | 'DEBUG' | 'TRACE'
export type SearchMode = 'index' | 'archive'

export interface ContextLine {
  timestamp: string
  level: LogLevel
  message: string
  fields: Record<string, unknown> | null
  line_offset: number
  is_match: boolean
}

export interface LineContext {
  before: ContextLine[]
  after: ContextLine[]
}

export interface LogSearchLine {
  timestamp: string
  level: LogLevel
  service: string
  message: string
  fields: Record<string, unknown> | null
  chunk_id: string
  line_offset: number
  deploy_id: number | null
  /** Container this line came from — lets the UI tag/group a combined view. */
  container_id?: string
  /** Worker node id (null/absent = control-plane-local). */
  node_id?: number | null
  /** Human-readable node name for display. */
  node_name?: string | null
  /** Raw grep -C surrounding lines, present only when contextLines > 0. */
  context?: LineContext | null
}

/** A distinct container/node/service available in the queried scope — used to
    populate the filter dropdowns with the full set of options regardless of the
    active container/node filter. */
export interface LogSource {
  container_id: string
  service: string
  node_id?: number | null
  node_name?: string | null
}

export interface SearchLogsResponse {
  lines: LogSearchLine[]
  next_cursor: string | null
  search_mode: SearchMode
  total_scanned: number
  /** Full set of sources for the scope (first page only). */
  available_sources?: LogSource[]
}

export interface LogSearchParams {
  projectId: number
  /**
   * When set, search an imported/managed external service's logs instead of a
   * project's. `projectId` is ignored server-side in this mode.
   */
  externalServiceId?: number
  startTime?: string
  endTime?: string
  levels?: LogLevel[]
  services?: string[]
  envs?: string[]
  text?: string
  cursor?: string
  pageSize?: number
  /** grep -C: raw lines to show before AND after each match (0 = off). */
  contextLines?: number
  /** Filter to logs emitted by a single deployment (deployments.id). */
  deployId?: number
  /** Filter to specific containers (Docker container IDs). Empty/undefined = all. */
  containerIds?: string[]
  /** Filter to specific worker nodes (node_id). Empty/undefined = all nodes. */
  nodeIds?: number[]
  /** Bump to force a brand-new query (busts the infinite-query cache so the
      viewer collapses back to a single newest page and re-tails). Ignored by
      the plain {@link useLogHistory} hook. */
  refreshKey?: number
}

// ── API call ───────────────────────────────────────────────────────────

async function searchLogs(params: LogSearchParams): Promise<SearchLogsResponse> {
  const body: Record<string, unknown> = {
    project_id: params.projectId,
  }

  if (params.externalServiceId != null)
    body.external_service_id = params.externalServiceId
  if (params.startTime) body.start_time = params.startTime
  if (params.endTime) body.end_time = params.endTime
  if (params.levels?.length) body.levels = params.levels
  if (params.services?.length) body.services = params.services
  if (params.envs?.length) body.envs = params.envs
  if (params.text) body.text = params.text
  if (params.cursor) body.cursor = params.cursor
  if (params.pageSize) body.page_size = params.pageSize
  if (params.contextLines && params.contextLines > 0)
    body.context_lines = params.contextLines
  if (params.deployId != null) body.deploy_id = params.deployId
  if (params.containerIds?.length) body.container_ids = params.containerIds
  if (params.nodeIds?.length) body.node_ids = params.nodeIds

  const response = await client.post({
    url: '/logs/search',
    body,
    security: [{ scheme: 'bearer', type: 'http' }],
  })

  return response.data as SearchLogsResponse
}

// ── React Query hook ───────────────────────────────────────────────────

export function useLogHistory(params: LogSearchParams, enabled = true) {
  return useQuery({
    queryKey: [
      'log-history',
      params.projectId,
      params.externalServiceId,
      params.startTime,
      params.endTime,
      params.levels,
      params.services,
      params.envs,
      params.text,
      params.cursor,
      params.pageSize,
      params.contextLines,
      params.deployId,
      params.containerIds,
      params.nodeIds,
    ],
    queryFn: () => searchLogs(params),
    enabled: enabled && (!!params.projectId || params.externalServiceId != null),
    staleTime: 1000 * 30, // 30 seconds
    placeholderData: keepPreviousData,
  })
}

/**
 * Infinite/tailing variant of {@link useLogHistory} for terminal-style viewers.
 *
 * The server bounds the first page (no cursor) to the *newest* `pageSize`
 * matches and returns them oldest→newest, so the caller can render newest at
 * the bottom and scroll to it on load. Each `fetchNextPage()` walks `next_cursor`
 * into strictly *older* lines — the caller prepends those at the top. Pages are
 * therefore concatenated as `[...pages].reverse().flatMap(p => p.lines)` to get
 * a single fully-ascending rope (oldest loaded at the top, newest at the bottom).
 *
 * Changing any filter (or bumping `refreshKey`) resets to a fresh newest page.
 */
export function useLogHistoryInfinite(
  params: Omit<LogSearchParams, 'cursor'>,
  enabled = true
) {
  return useInfiniteQuery({
    queryKey: [
      'log-history-infinite',
      params.projectId,
      params.externalServiceId,
      params.startTime,
      params.endTime,
      params.levels,
      params.services,
      params.envs,
      params.text,
      params.pageSize,
      params.contextLines,
      params.deployId,
      params.containerIds,
      params.nodeIds,
      params.refreshKey,
    ],
    queryFn: ({ pageParam }) =>
      searchLogs({ ...params, cursor: pageParam ?? undefined }),
    initialPageParam: undefined as string | undefined,
    // lastPage is the most recently fetched (oldest) page; its next_cursor
    // points at the next-older page. Undefined stops the "load older" walk.
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    enabled: enabled && (!!params.projectId || params.externalServiceId != null),
    staleTime: 1000 * 30,
  })
}

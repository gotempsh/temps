import { client } from '@/api/client/client.gen'
import { useQuery, keepPreviousData } from '@tanstack/react-query'

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
  /** Raw grep -C surrounding lines, present only when contextLines > 0. */
  context?: LineContext | null
}

export interface SearchLogsResponse {
  lines: LogSearchLine[]
  next_cursor: string | null
  search_mode: SearchMode
  total_scanned: number
}

export interface LogSearchParams {
  projectId: number
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
}

// ── API call ───────────────────────────────────────────────────────────

async function searchLogs(params: LogSearchParams): Promise<SearchLogsResponse> {
  const body: Record<string, unknown> = {
    project_id: params.projectId,
  }

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
    ],
    queryFn: () => searchLogs(params),
    enabled: enabled && !!params.projectId,
    staleTime: 1000 * 30, // 30 seconds
    placeholderData: keepPreviousData,
  })
}

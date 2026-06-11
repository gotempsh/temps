'use client'

import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getProjectDeploymentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { DateRangePicker } from '@/components/ui/date-range-picker'
import type { DateRange } from 'react-day-picker'
import { cn } from '@/lib/utils'
import {
  ContextLine,
  LogLevel,
  LogSearchLine,
  useLogHistory,
} from '@/hooks/useLogHistory'
import { useQuery } from '@tanstack/react-query'
import { useVirtualizer } from '@tanstack/react-virtual'
import { useSearchParams } from 'react-router-dom'
import AnsiToHtml from 'ansi-to-html'
import {
  AlertCircle,
  AlignVerticalSpaceAround,
  ArrowUp,
  Clock,
  Columns3,
  Loader2,
  Search,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'

const ansiConverter = new AnsiToHtml({
  fg: 'var(--foreground)',
  bg: 'var(--background)',
  newline: false,
  escapeXML: true,
})

// Render a log message to HTML with ANSI colors, then layer the matched search
// term as a <mark> on top — same pattern as the live-tail viewer (log-viewer.tsx)
// so highlighting looks identical across both log surfaces. The replace runs on
// the already-escaped HTML, in plain text, so it can't mangle ANSI span markup.
function renderMessageHtml(message: string, searchTerm?: string): string {
  const html = ansiConverter.toHtml(message)
  if (!searchTerm) return html
  const escaped = searchTerm.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return html.replace(
    new RegExp(`(${escaped})`, 'gi'),
    '<mark class="bg-yellow-200 dark:bg-yellow-800 rounded px-1">$1</mark>',
  )
}

const LOG_LEVEL_OPTIONS: LogLevel[] = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE']

const LEVEL_COLORS: Record<LogLevel, string> = {
  ERROR: 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/20',
  WARN: 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 border-yellow-500/20',
  INFO: 'bg-blue-500/15 text-blue-700 dark:text-blue-400 border-blue-500/20',
  DEBUG: 'bg-zinc-500/15 text-zinc-700 dark:text-zinc-400 border-zinc-500/20',
  TRACE: 'bg-zinc-400/15 text-zinc-500 dark:text-zinc-500 border-zinc-400/20',
}

// Server enforces a 24h cap on full-text search (MAX_FULLTEXT_HOURS in
// search.rs). The 7d/30d presets and any custom range over 24h are fine for
// level/service filters but the search box must be disabled to avoid a 400
// from the backend.
const FULLTEXT_MAX_HOURS = 24

const TIME_RANGES: Array<{ value: string; label: string; hours: number }> = [
  { value: '15m', label: 'Last 15 min', hours: 0.25 },
  { value: '1h', label: 'Last 1 hour', hours: 1 },
  { value: '6h', label: 'Last 6 hours', hours: 6 },
  { value: '24h', label: 'Last 24 hours', hours: 24 },
  { value: '7d', label: 'Last 7 days', hours: 24 * 7 },
  { value: '30d', label: 'Last 30 days', hours: 24 * 30 },
]

const CUSTOM_RANGE_VALUE = 'custom'

// Minimal shape we read off a DeploymentResponse for the filter dropdown and
// the deploy-window default. Kept local so the component doesn't depend on the
// full generated type beyond these fields.
interface DeploymentLike {
  id: number
  branch?: string | null
  commit_hash?: string | null
  commit_message?: string | null
  tag?: string | null
  created_at: number
  started_at?: number | null
  finished_at?: number | null
}

// Compact one-line label for a deployment, e.g. "#171 · feat/ovh-version · a1b2c3d".
// Falls back through tag → first line of commit message → bare #id so there's
// always something readable.
function deploymentLabel(d: DeploymentLike): string {
  const parts: string[] = [`#${d.id}`]
  const ref = d.branch || d.tag
  if (ref) parts.push(ref)
  if (d.commit_hash) {
    parts.push(d.commit_hash.slice(0, 7))
  } else if (d.commit_message) {
    parts.push(d.commit_message.split('\n')[0].slice(0, 40))
  }
  return parts.join(' · ')
}

function presetTimeRange(range: string): { start: string; end: string } {
  const now = new Date()
  const end = now.toISOString()
  const found = TIME_RANGES.find((r) => r.value === range)
  const hours = found?.hours ?? 1
  const start = new Date(now.getTime() - hours * 60 * 60 * 1000).toISOString()
  return { start, end }
}


interface ColumnVisibility {
  timestamp: boolean
  level: boolean
  service: boolean
}

const CONTEXT_MIN = 0
const CONTEXT_MAX = 50

// A rendered row is one of:
//  - 'match'     : a search hit (full styling, highlighted)
//  - 'context'   : a raw neighbor line shown by grep -C (dimmed)
//  - 'separator' : a thin "⋯" divider between non-contiguous blocks
type RenderRow =
  | { kind: 'match'; key: string; line: LogSearchLine }
  | {
      kind: 'context'
      key: string
      timestamp: string
      level: LogLevel
      message: string
      service: string
    }
  | { kind: 'separator'; key: string }

function formatTs(timestamp: string): string {
  const d = new Date(timestamp)
  const base = d.toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
  return `${base}.${String(d.getMilliseconds()).padStart(3, '0')}`
}

// Flatten the match rope into render rows, interleaving each match's grep -C
// context. Contiguous lines (same chunk, adjacent offsets) render as one block;
// a gap inserts a separator. Dedup on (chunk_id, line_offset) guards against
// any residual window overlap the backend didn't merge.
function buildRenderRows(lines: LogSearchLine[]): RenderRow[] {
  const rows: RenderRow[] = []
  const seen = new Set<string>()
  // Track the last emitted (chunk_id, offset) to decide separators within a
  // contiguous run. Reset across matches that don't carry context.
  let lastChunk: string | null = null
  let lastOffset: number | null = null

  // Context lines come from the same chunk as their match, and a chunk is a
  // single container/service — so a neighbor's service is always the match's
  // service. The backend's ContextLine doesn't carry it, so we inherit it here
  // to keep the deployment column populated for context rows too.
  const pushContext = (chunkId: string, service: string, c: ContextLine) => {
    const key = `${chunkId}:${c.line_offset}`
    if (seen.has(key)) return
    seen.add(key)
    if (
      lastChunk === chunkId &&
      lastOffset !== null &&
      c.line_offset > lastOffset + 1
    ) {
      rows.push({ kind: 'separator', key: `sep-${key}` })
    }
    rows.push({
      kind: 'context',
      key,
      timestamp: c.timestamp,
      level: c.level,
      message: c.message,
      service,
    })
    lastChunk = chunkId
    lastOffset = c.line_offset
  }

  for (const line of lines) {
    const ctx = line.context
    if (ctx?.before?.length) {
      // A new block start that isn't contiguous with the previous row → divider.
      if (
        rows.length > 0 &&
        !(lastChunk === line.chunk_id &&
          lastOffset !== null &&
          ctx.before[0].line_offset <= lastOffset + 1)
      ) {
        rows.push({ kind: 'separator', key: `sep-pre-${line.chunk_id}:${line.line_offset}` })
        lastChunk = null
        lastOffset = null
      }
      for (const c of ctx.before) pushContext(line.chunk_id, line.service, c)
    }

    const matchKey = `${line.chunk_id}:${line.line_offset}`
    if (!seen.has(matchKey)) {
      seen.add(matchKey)
      // Separator if the match isn't contiguous with the previous row.
      if (
        rows.length > 0 &&
        !(lastChunk === line.chunk_id &&
          lastOffset !== null &&
          line.line_offset <= lastOffset + 1)
      ) {
        rows.push({ kind: 'separator', key: `sep-m-${matchKey}` })
      }
      rows.push({ kind: 'match', key: matchKey, line })
      lastChunk = line.chunk_id
      lastOffset = line.line_offset
    }

    if (ctx?.after?.length) {
      for (const c of ctx.after) pushContext(line.chunk_id, line.service, c)
    }
  }

  return rows
}

function HistoryLogRow({
  row,
  columns,
  searchTerm,
}: {
  row: RenderRow
  columns: ColumnVisibility
  searchTerm?: string
}) {
  if (row.kind === 'separator') {
    // Gap between two non-adjacent match windows (like grep's `--`). Not a
    // truncation — the context for each match is complete; these blocks simply
    // aren't next to each other in the log stream.
    return (
      <div className="flex items-center gap-2 px-2 py-1 select-none text-[10px] text-muted-foreground/50">
        <span className="h-px flex-1 bg-border" />
        <span className="shrink-0">gap in logs</span>
        <span className="h-px flex-1 bg-border" />
      </div>
    )
  }

  const isContext = row.kind === 'context'
  const isMatch = row.kind === 'match'
  const timestamp = isContext ? row.timestamp : row.line.timestamp
  const level = isContext ? row.level : row.line.level
  const message = isContext ? row.message : row.line.message
  const service = isContext ? row.service : row.line.service
  const ts = formatTs(timestamp)

  return (
    <div
      className={cn(
        'flex items-start gap-2 py-0.5 px-2 font-mono text-xs hover:bg-muted/50',
        // Datadog-style, in our visual language: the matched search term is
        // highlighted inline via <mark> (renderMessageHtml), so the message
        // itself shows why a line matched. The match ("origin") row gets only a
        // restrained left accent + faint tint to anchor it; context lines are
        // recessed. We let the <mark> do the work rather than shouting the row.
        isContext && 'opacity-60',
        isMatch && 'border-l-2 border-l-primary/70 bg-primary/[0.04] pl-[6px]',
      )}
    >
      {columns.timestamp && (
        <span className="text-muted-foreground shrink-0 tabular-nums w-[85px]">
          {ts}
        </span>
      )}
      {columns.level && (
        <Badge
          variant="outline"
          className={cn(
            'shrink-0 text-[10px] font-medium px-1.5 py-0 h-[18px] leading-[18px] rounded-sm',
            LEVEL_COLORS[level] ?? LEVEL_COLORS.INFO,
          )}
        >
          {level}
        </Badge>
      )}
      {columns.service && (
        <span className="text-muted-foreground shrink-0 w-[70px] truncate">
          {service}
        </span>
      )}
      <span
        className="whitespace-pre-wrap break-all min-w-0 flex-1"
        dangerouslySetInnerHTML={{
          __html: renderMessageHtml(message, searchTerm),
        }}
      />
    </div>
  )
}

export default function HistoryLogViewer({
  project,
}: {
  project: ProjectResponse
}) {
  const [searchParams, setSearchParams] = useSearchParams()
  const [selectedEnv, setSelectedEnv] = useState<string | undefined>()
  const [selectedService, setSelectedService] = useState<string | undefined>()
  // Deployment filter (deployments.id). Seeded once from the ?deploy_id= URL
  // param so a deep link lands pre-filtered; thereafter the URL is kept in sync
  // by the effect below.
  const [selectedDeploy, setSelectedDeploy] = useState<number | undefined>(
    () => {
      const raw = searchParams.get('deploy_id')
      if (!raw) return undefined
      const n = Number.parseInt(raw, 10)
      return Number.isFinite(n) ? n : undefined
    },
  )
  const [selectedLevels, setSelectedLevels] = useState<LogLevel[]>([])
  const [searchText, setSearchText] = useState('')
  const [debouncedText, setDebouncedText] = useState('')
  const [timeRange, setTimeRange] = useState('24h')
  // Custom range, only meaningful when timeRange === CUSTOM_RANGE_VALUE.
  // Stored as DateRange (from react-day-picker) so the existing
  // DateRangePicker component can both read and write it without translation.
  const [customRange, setCustomRange] = useState<DateRange | undefined>()
  // Older pages loaded on demand. The first page comes from useLogHistory.
  // When the user clicks "Load older", we fetch the next page using the
  // current oldest line's cursor and append the result here. Filter/range
  // changes clear this back to []; see the filterKey effect below.
  const [olderPages, setOlderPages] = useState<LogSearchLine[][]>([])
  const [olderCursor, setOlderCursor] = useState<string | undefined>()
  const [loadingOlder, setLoadingOlder] = useState(false)
  const [olderError, setOlderError] = useState<string | undefined>()
  const [olderExhausted, setOlderExhausted] = useState(false)
  // Column visibility — persisted to localStorage so the user's choice
  // survives navigation. Defaults to all visible. The "deployment" column
  // is the per-line service name (container/service that emitted the log).
  const [columns, setColumns] = useState<ColumnVisibility>(() => {
    if (typeof window === 'undefined') {
      return { timestamp: true, level: true, service: true }
    }
    try {
      const raw = window.localStorage.getItem('temps.history-log.columns')
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<ColumnVisibility>
        return {
          timestamp: parsed.timestamp ?? true,
          level: parsed.level ?? true,
          service: parsed.service ?? true,
        }
      }
    } catch {
      // Ignore corrupted storage and fall through to defaults.
    }
    return { timestamp: true, level: true, service: true }
  })
  useEffect(() => {
    try {
      window.localStorage.setItem(
        'temps.history-log.columns',
        JSON.stringify(columns),
      )
    } catch {
      // Storage may be unavailable (private mode, quota); not worth surfacing.
    }
  }, [columns])

  // grep -C: raw lines to show before AND after each match. 0 = off. Persisted
  // like columns so the user's choice survives navigation.
  const [contextLines, setContextLines] = useState<number>(() => {
    if (typeof window === 'undefined') return 0
    try {
      const raw = window.localStorage.getItem('temps.history-log.context-lines')
      if (raw !== null) {
        const n = Number.parseInt(raw, 10)
        if (Number.isFinite(n)) return Math.min(Math.max(n, CONTEXT_MIN), CONTEXT_MAX)
      }
    } catch {
      // Ignore corrupted storage.
    }
    return 0
  })
  useEffect(() => {
    try {
      window.localStorage.setItem(
        'temps.history-log.context-lines',
        String(contextLines),
      )
    } catch {
      // Storage may be unavailable; not worth surfacing.
    }
  }, [contextLines])

  const parentRef = useRef<HTMLDivElement>(null)
  // Include the custom range in the filterKey so picking a different window
  // resets the older-pages rope. ms keys keep the string compact.
  const customRangeKey =
    timeRange === CUSTOM_RANGE_VALUE && customRange?.from && customRange?.to
      ? `${customRange.from.getTime()}-${customRange.to.getTime()}`
      : ''
  const filterKey = `${selectedEnv}-${selectedService}-${selectedDeploy}-${selectedLevels.join(',')}-${debouncedText}-${timeRange}-${customRangeKey}-${contextLines}`
  const prevFilterKeyRef = useRef(filterKey)

  // Debounce search text
  useEffect(() => {
    const timer = setTimeout(() => setDebouncedText(searchText), 400)
    return () => clearTimeout(timer)
  }, [searchText])

  // Reset the older-pages rope whenever any filter changes — otherwise we'd
  // splice unrelated logs from a stale level/text/range into the current view.
  if (filterKey !== prevFilterKeyRef.current) {
    prevFilterKeyRef.current = filterKey
    setOlderPages([])
    setOlderCursor(undefined)
    setOlderExhausted(false)
    setOlderError(undefined)
  }

  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
  })

  useEffect(() => {
    if (environments?.length && !selectedEnv) {
      setSelectedEnv(String(environments[0].id))
    }
  }, [environments, selectedEnv])

  // Project deployments, used to populate the Deployment filter dropdown. We
  // pull a generous page so older deployments are still selectable; the list is
  // small per-project and this is a metadata-only call.
  const { data: deploymentsData } = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: { per_page: 100 },
    }),
  })
  const deployments = useMemo<DeploymentLike[]>(
    () =>
      (deploymentsData?.deployments ?? [])
        .map((d) => ({
          id: d.id,
          branch: d.branch,
          commit_hash: d.commit_hash,
          commit_message: d.commit_message,
          tag: d.tag,
          created_at: d.created_at,
          started_at: d.started_at,
          finished_at: d.finished_at,
        }))
        .sort((a, b) => b.id - a.id),
    [deploymentsData],
  )

  // Keep ?deploy_id= in the URL in sync with the selection so the view is
  // shareable / refresh-stable. Uses replace so it doesn't pollute history.
  useEffect(() => {
    const current = searchParams.get('deploy_id')
    const next = selectedDeploy != null ? String(selectedDeploy) : null
    if (current === next) return
    const params = new URLSearchParams(searchParams)
    if (next === null) {
      params.delete('deploy_id')
    } else {
      params.set('deploy_id', next)
    }
    setSearchParams(params, { replace: true })
  }, [selectedDeploy, searchParams, setSearchParams])

  // When the user picks a deployment, default the time window to that
  // deployment's lifespan so they don't have to guess timestamps. Only fires on
  // an actual *change* of selection (tracked via a ref), leaving the range
  // editable afterward. A still-running deploy (no finished_at) spans up to now.
  const lastDeployWindowRef = useRef<number | undefined>(selectedDeploy)
  useEffect(() => {
    if (selectedDeploy === lastDeployWindowRef.current) return
    lastDeployWindowRef.current = selectedDeploy
    if (selectedDeploy == null) return
    const d = deployments.find((x) => x.id === selectedDeploy)
    if (!d) return
    // Deployment timestamps come from the API as epoch MILLISECONDS (e.g.
    // created_at = 1777651454430), so they feed straight into new Date(ms) — do
    // NOT multiply by 1000.
    const startMs = d.started_at ?? d.created_at
    if (!startMs) return
    const from = new Date(startMs)
    const to = d.finished_at ? new Date(d.finished_at) : new Date()
    setCustomRange({ from, to })
    setTimeRange(CUSTOM_RANGE_VALUE)
  }, [selectedDeploy, deployments])

  // Resolve {start, end} from either the active preset or a custom DateRange.
  // For presets we recompute on every timeRange change (presetTimeRange calls
  // new Date()), so the memo's job is to avoid creating new strings on every
  // render — only when the inputs actually change. When custom is active but
  // not yet fully picked (start or end missing) we fall back to a 1h window
  // so the query stays well-formed instead of erroring.
  const { start, end } = useMemo(() => {
    if (timeRange === CUSTOM_RANGE_VALUE) {
      if (customRange?.from && customRange?.to) {
        return {
          start: customRange.from.toISOString(),
          end: customRange.to.toISOString(),
        }
      }
      return presetTimeRange('1h')
    }
    return presetTimeRange(timeRange)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [timeRange, customRangeKey])

  // Effective range duration for the fulltext-disabled gate. Custom ranges
  // get their actual span; presets fall back to the table; unknown defaults
  // to 1h so we never lock the user out unexpectedly.
  const currentRangeHours = useMemo(() => {
    if (timeRange === CUSTOM_RANGE_VALUE) {
      if (customRange?.from && customRange?.to) {
        const ms = customRange.to.getTime() - customRange.from.getTime()
        return Math.max(0, ms / (60 * 60 * 1000))
      }
      return 1
    }
    return TIME_RANGES.find((r) => r.value === timeRange)?.hours ?? 1
  }, [timeRange, customRange])
  const fulltextDisabled = currentRangeHours > FULLTEXT_MAX_HOURS

  // Hard-clear the search box when a long range is picked so the disabled
  // input doesn't quietly send a stale text filter that the server rejects.
  useEffect(() => {
    if (fulltextDisabled && searchText.length > 0) {
      setSearchText('')
    }
  }, [fulltextDisabled, searchText])

  const { data, isLoading, isFetching, error } = useLogHistory(
    {
      projectId: project.id,
      startTime: start,
      endTime: end,
      levels: selectedLevels.length > 0 ? selectedLevels : undefined,
      envs: selectedEnv ? [selectedEnv] : undefined,
      services: selectedService ? [selectedService] : undefined,
      deployId: selectedDeploy,
      text: !fulltextDisabled && debouncedText ? debouncedText : undefined,
      // Frontend asks for 500 per page; server caps at 2000.
      pageSize: 500,
      contextLines: contextLines > 0 ? contextLines : undefined,
    },
    !!project.id,
  )

  // Build the rendered rope in ASC order (oldest at index 0, newest at end)
  // so the UI renders terminal-style with newest at the bottom.
  //
  // Wire shape from the server: each page is already ASC. The first page
  // contains the *newest* slab in the time window. Older pages, fetched as
  // the user scrolls up, contain progressively earlier slabs.
  //
  // olderPages is in fetch order: [page2, page3, ...]. page2 covers logs
  // immediately older than page1; page3 covers logs immediately older than
  // page2; etc. To get a single ASC vec we walk olderPages from the END
  // backward (oldest fetched first) then append the first page at the end.
  const lines = useMemo(() => {
    const firstPage = data?.lines ?? []
    if (olderPages.length === 0) return firstPage
    const olderFlat: LogSearchLine[] = []
    for (let i = olderPages.length - 1; i >= 0; i--) {
      olderFlat.push(...olderPages[i])
    }
    return olderFlat.concat(firstPage)
  }, [data?.lines, olderPages])

  // The cursor we use for "load older" — once we've loaded extra pages,
  // the server's last-emitted cursor lives in olderCursor; before that,
  // it's data.next_cursor from the first-page response.
  const effectiveNextCursor =
    olderCursor !== undefined ? olderCursor : data?.next_cursor ?? null

  // Flatten matches + their grep -C context into a single list of render rows.
  // When contextLines is 0 every row is a plain match, so this is a cheap 1:1
  // map and the view behaves exactly as before.
  const renderRows = useMemo(() => buildRenderRows(lines), [lines])

  const virtualizer = useVirtualizer({
    count: renderRows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 22,
    overscan: 20,
  })

  const toggleLevel = useCallback((level: LogLevel) => {
    setSelectedLevels((prev) =>
      prev.includes(level) ? prev.filter((l) => l !== level) : [...prev, level],
    )
  }, [])

  const handleLoadOlder = useCallback(async () => {
    if (!effectiveNextCursor || loadingOlder) return
    setLoadingOlder(true)
    setOlderError(undefined)
    try {
      const body: Record<string, unknown> = {
        project_id: project.id,
        start_time: start,
        end_time: end,
        cursor: effectiveNextCursor,
        page_size: 500,
      }
      if (selectedLevels.length > 0) body.levels = selectedLevels
      if (selectedEnv) body.envs = [selectedEnv]
      if (selectedService) body.services = [selectedService]
      if (selectedDeploy != null) body.deploy_id = selectedDeploy
      if (!fulltextDisabled && debouncedText) body.text = debouncedText
      if (contextLines > 0) body.context_lines = contextLines

      const res = await fetch('/api/logs/search', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify(body),
      })
      if (!res.ok) {
        throw new Error(`Server returned ${res.status}`)
      }
      const json = (await res.json()) as {
        lines: LogSearchLine[]
        next_cursor: string | null
      }
      if (json.lines.length === 0) {
        setOlderExhausted(true)
      } else {
        setOlderPages((prev) => [...prev, json.lines])
      }
      setOlderCursor(json.next_cursor ?? undefined)
      if (!json.next_cursor) {
        setOlderExhausted(true)
      }
    } catch (e) {
      setOlderError(e instanceof Error ? e.message : 'Failed to load older logs')
    } finally {
      setLoadingOlder(false)
    }
  }, [
    effectiveNextCursor,
    loadingOlder,
    project.id,
    start,
    end,
    selectedLevels,
    selectedEnv,
    selectedService,
    selectedDeploy,
    fulltextDisabled,
    debouncedText,
    contextLines,
  ])

  const totalMatched = data?.total_scanned ?? 0
  const showingCount = lines.length
  const canLoadOlder = !!effectiveNextCursor && !olderExhausted

  // Auto-load older logs when the sentinel enters the scroll viewport.
  // Lines are ASC (newest at the bottom) so "older" lives at the TOP of
  // the log pane — the sentinel sits above all rendered lines and fires
  // when the user has scrolled close to the top of the rope. Stops when:
  //   - we're already fetching (loadingOlder)
  //   - the cursor is exhausted (canLoadOlder = false)
  //   - the previous attempt errored (olderError) — user must click to retry
  // The manual button stays mounted so error-retry and visual progress are
  // both addressable; it just rarely needs to be clicked in practice.
  const sentinelRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const sentinel = sentinelRef.current
    const root = parentRef.current
    if (!sentinel || !root) return
    if (!canLoadOlder || loadingOlder || olderError) return

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            void handleLoadOlder()
          }
        }
      },
      {
        root,
        // Pre-fire 200px before the sentinel reaches the top of the
        // viewport so the next page is already arriving by the time the
        // user scrolls to the top of the rope.
        rootMargin: '200px 0px 0px 0px',
        threshold: 0,
      },
    )
    observer.observe(sentinel)
    return () => observer.disconnect()
  }, [canLoadOlder, loadingOlder, olderError, handleLoadOlder])

  // Pin scroll to the bottom on the very first page render. After that, the
  // user owns the scroll position — we don't auto-snap on subsequent
  // updates (especially not on prepended older pages, where the goal is
  // exactly the opposite: keep the user's reading position fixed even as
  // new rows shift in above).
  const initialScrolledRef = useRef(false)
  useEffect(() => {
    if (initialScrolledRef.current) return
    const root = parentRef.current
    if (!root) return
    if ((data?.lines?.length ?? 0) === 0) return
    // Defer to next frame so the virtualizer has measured first-page rows.
    const id = requestAnimationFrame(() => {
      root.scrollTop = root.scrollHeight
      initialScrolledRef.current = true
    })
    return () => cancelAnimationFrame(id)
  }, [data?.lines])

  // Reset the "first scroll" flag when filters change, so a freshly-fetched
  // first page anchors to the bottom again.
  useEffect(() => {
    initialScrolledRef.current = false
  }, [filterKey])

  // Preserve scroll position when older pages prepend at the top. Without
  // this, scrollTop stays at its absolute pixel value while N new rows are
  // inserted above it — which yanks the user's eye downward to whatever
  // line is now at that pixel offset. Capture scrollHeight before the
  // append, then bump scrollTop by the delta after the DOM settles.
  const prevScrollHeightRef = useRef<number | null>(null)
  const olderPagesCountRef = useRef(0)
  useEffect(() => {
    const root = parentRef.current
    if (!root) return
    const prev = olderPagesCountRef.current
    const next = olderPages.length
    olderPagesCountRef.current = next

    // A fetch began (loadingOlder true) — snapshot scrollHeight.
    if (loadingOlder && prevScrollHeightRef.current == null) {
      prevScrollHeightRef.current = root.scrollHeight
      return
    }
    // A new older page just landed — adjust scrollTop by the height delta.
    if (next > prev && prevScrollHeightRef.current != null) {
      const before = prevScrollHeightRef.current
      prevScrollHeightRef.current = null
      // Wait one frame for the virtualizer to grow the spacer for the
      // newly-prepended rows, then offset scrollTop by the delta so the
      // user's read position is visually stable.
      requestAnimationFrame(() => {
        const delta = root.scrollHeight - before
        if (delta > 0) {
          root.scrollTop = root.scrollTop + delta
        }
      })
    }
  }, [olderPages, loadingOlder])

  return (
    <TooltipProvider delayDuration={150}>
      <div className="p-4 space-y-4">
        {/* Filters */}
        <div className="flex flex-col sm:flex-row gap-3 items-start sm:items-center">
          <Select value={selectedEnv} onValueChange={setSelectedEnv}>
            <SelectTrigger className="w-full sm:w-[200px]">
              <SelectValue placeholder="All environments" />
            </SelectTrigger>
            <SelectContent>
              {environments?.map((env) => (
                <SelectItem key={env.id} value={String(env.id)}>
                  {env.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          <Select
            value={selectedService ?? 'all'}
            onValueChange={(v) =>
              setSelectedService(v === 'all' ? undefined : v)
            }
          >
            <SelectTrigger className="w-full sm:w-auto sm:max-w-[300px]">
              <SelectValue placeholder="All services" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All services</SelectItem>
              {Array.from(
                new Set(
                  (data?.lines ?? [])
                    .map((l) => l.service)
                    .filter((s) => s && s !== 'unknown'),
                ),
              )
                .sort()
                .map((service) => (
                  <SelectItem key={service} value={service}>
                    {service}
                  </SelectItem>
                ))}
            </SelectContent>
          </Select>

          {/* Deployment filter. Selecting one scopes the logs to the chunks
              tagged with that deployment's id AND defaults the time window to
              the deploy's lifespan (see the effect above), so the user doesn't
              have to know the timestamps. Mirrors ?deploy_id= in the URL. */}
          <Select
            value={selectedDeploy != null ? String(selectedDeploy) : 'all'}
            onValueChange={(v) =>
              setSelectedDeploy(v === 'all' ? undefined : Number.parseInt(v, 10))
            }
          >
            <SelectTrigger className="w-full sm:w-auto sm:max-w-[320px]">
              <SelectValue placeholder="All deployments" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All deployments</SelectItem>
              {deployments.map((d) => (
                <SelectItem key={d.id} value={String(d.id)}>
                  {deploymentLabel(d)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          {/* Time range: presets + a Custom range… entry. When the user
              picks Custom range… we seed the DateRangePicker with the
              previously-active preset window (smooth handoff) and reveal
              the picker next to the select. The DateRangePicker is
              self-contained — its trigger button is what the user clicks
              to actually edit the range, while this Select stays as the
              single source of truth for which mode is active. */}
          <div className="flex items-center gap-1">
            <Select
              value={timeRange}
              onValueChange={(v) => {
                if (
                  v === CUSTOM_RANGE_VALUE &&
                  (!customRange?.from || !customRange?.to)
                ) {
                  // Seed with the previous preset window so the calendar
                  // opens at a sensible position. Only seeds when no custom
                  // range is set yet — preserves a previously-picked window
                  // across preset round-trips.
                  const seedKey = TIME_RANGES.find((r) => r.value === timeRange)
                    ? timeRange
                    : '1h'
                  const seed = presetTimeRange(seedKey)
                  setCustomRange({
                    from: new Date(seed.start),
                    to: new Date(seed.end),
                  })
                }
                setTimeRange(v)
              }}
            >
              <SelectTrigger className="w-[180px]">
                <Clock className="h-3.5 w-3.5 mr-2 text-muted-foreground shrink-0" />
                <SelectValue>
                  {timeRange === CUSTOM_RANGE_VALUE
                    ? 'Custom range'
                    : TIME_RANGES.find((r) => r.value === timeRange)?.label}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {TIME_RANGES.map((r) => (
                  <SelectItem key={r.value} value={r.value}>
                    {r.label}
                  </SelectItem>
                ))}
                <SelectItem value={CUSTOM_RANGE_VALUE}>
                  Custom range…
                </SelectItem>
              </SelectContent>
            </Select>

            {timeRange === CUSTOM_RANGE_VALUE && (
              <DateRangePicker
                date={customRange}
                onDateChange={setCustomRange}
                showTime
                className="w-[280px]"
              />
            )}
          </div>

          <Tooltip>
            <TooltipTrigger asChild>
              <div className="relative flex-1 min-w-[200px]">
                <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                <Input
                  placeholder={
                    fulltextDisabled
                      ? 'Search disabled for ranges over 24h'
                      : 'Search log messages...'
                  }
                  value={searchText}
                  onChange={(e) => setSearchText(e.target.value)}
                  className="pl-9 w-full"
                  disabled={fulltextDisabled}
                />
              </div>
            </TooltipTrigger>
            {fulltextDisabled && (
              <TooltipContent side="bottom">
                Full-text search is limited to ranges of 24 hours or less.
                Narrow the time range to search inside log messages.
              </TooltipContent>
            )}
          </Tooltip>

          {/* grep -C: surrounding lines around each match. 0 = off. Capped at
              50 each side (matches the server cap). */}
          <Tooltip>
            <TooltipTrigger asChild>
              <div className="flex items-center gap-1.5 shrink-0">
                <AlignVerticalSpaceAround className="h-3.5 w-3.5 text-muted-foreground" />
                <Input
                  type="number"
                  min={CONTEXT_MIN}
                  max={CONTEXT_MAX}
                  step={1}
                  value={contextLines}
                  onChange={(e) => {
                    const n = Number.parseInt(e.target.value, 10)
                    const next = Number.isFinite(n) ? n : 0
                    setContextLines(
                      Math.min(Math.max(next, CONTEXT_MIN), CONTEXT_MAX),
                    )
                  }}
                  aria-label="Surrounding context lines"
                  className="w-[64px] tabular-nums"
                />
                <span className="text-xs text-muted-foreground hidden sm:inline">
                  ± lines
                </span>
              </div>
            </TooltipTrigger>
            <TooltipContent side="bottom">
              Show this many raw log lines before and after each match
              (grep&nbsp;-C). 0 disables it. Max {CONTEXT_MAX}.
            </TooltipContent>
          </Tooltip>

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm" className="gap-1.5">
                <Columns3 className="h-3.5 w-3.5" />
                Columns
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-44">
              <DropdownMenuLabel>Show columns</DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuCheckboxItem
                checked={columns.timestamp}
                onCheckedChange={(v) =>
                  setColumns((c) => ({ ...c, timestamp: v === true }))
                }
              >
                Timestamp
              </DropdownMenuCheckboxItem>
              <DropdownMenuCheckboxItem
                checked={columns.level}
                onCheckedChange={(v) =>
                  setColumns((c) => ({ ...c, level: v === true }))
                }
              >
                Level
              </DropdownMenuCheckboxItem>
              <DropdownMenuCheckboxItem
                checked={columns.service}
                onCheckedChange={(v) =>
                  setColumns((c) => ({ ...c, service: v === true }))
                }
              >
                Deployment
              </DropdownMenuCheckboxItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        {/* Level filter chips */}
        <div className="flex gap-1.5 flex-wrap">
          {LOG_LEVEL_OPTIONS.map((level) => (
            <button
              type="button"
              key={level}
              onClick={() => toggleLevel(level)}
              className={cn(
                'px-2.5 py-0.5 text-xs font-medium rounded-full border transition-colors',
                selectedLevels.includes(level)
                  ? LEVEL_COLORS[level]
                  : 'bg-muted/50 text-muted-foreground border-border hover:bg-muted',
              )}
            >
              {level}
            </button>
          ))}
          {selectedLevels.length > 0 && (
            <button
              type="button"
              onClick={() => setSelectedLevels([])}
              className="px-2.5 py-0.5 text-xs text-muted-foreground hover:text-foreground"
            >
              Clear
            </button>
          )}
        </div>

        {/* Error state */}
        {error && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Failed to load logs: {error.message}
            </AlertDescription>
          </Alert>
        )}

        {/* Results header */}
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <div className="flex items-center gap-2">
            {isFetching && <Loader2 className="h-3 w-3 animate-spin" />}
            <span>
              Showing {showingCount.toLocaleString()}
              {totalMatched > showingCount
                ? ` of ${totalMatched.toLocaleString()}+`
                : ''}{' '}
              {showingCount === 1 ? 'match' : 'matches'}
              {contextLines > 0 && (
                <span className="text-muted-foreground/70">
                  {' '}· ±{contextLines} context lines
                </span>
              )}
            </span>
          </div>
        </div>

        {/* Log lines */}
        <div className="border rounded-lg bg-background">
          {isLoading && !data ? (
            <div className="h-[500px] flex items-center justify-center">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : lines.length === 0 ? (
            <div className="h-[500px] flex items-center justify-center text-muted-foreground">
              <div className="text-center">
                <AlertCircle className="h-10 w-10 mx-auto mb-3 opacity-40" />
                <p className="text-sm">No logs found for the selected filters</p>
                <p className="text-xs mt-1">
                  Try adjusting the time range or clearing filters
                </p>
              </div>
            </div>
          ) : (
            <div
              ref={parentRef}
              className="h-[500px] overflow-auto select-text"
            >
              {/* Sentinel ABOVE the virtualizer spacer. ASC ordering means
                  older logs live at the top, so the auto-load-older trigger
                  belongs at the top of the rope. IntersectionObserver fires
                  handleLoadOlder when the sentinel scrolls into view. The
                  button-shaped content is the manual fallback (error retry,
                  accessibility). Must be inside parentRef so the observer's
                  root can detect intersection during scroll. */}
              <div
                ref={sentinelRef}
                className="border-b flex items-center justify-center px-3 py-2"
              >
                {olderError ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 text-xs gap-1.5 text-destructive"
                    onClick={handleLoadOlder}
                  >
                    Retry — {olderError}
                  </Button>
                ) : olderExhausted || !canLoadOlder ? (
                  <span className="text-xs text-muted-foreground">
                    Beginning of logs in this time range
                  </span>
                ) : loadingOlder ? (
                  <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <Loader2 className="h-3 w-3 animate-spin" />
                    Loading older logs...
                  </span>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 text-xs gap-1.5"
                    onClick={handleLoadOlder}
                  >
                    <ArrowUp className="h-3 w-3" />
                    Load older
                  </Button>
                )}
              </div>

              {/* Virtualizer spacer: absolute-positioned rows live inside,
                  laid out top→bottom in ASC timestamp order so the newest
                  line ends up at the bottom of the scroll viewport. */}
              <div
                style={{
                  height: `${virtualizer.getTotalSize()}px`,
                  width: '100%',
                  position: 'relative',
                }}
              >
                {virtualizer.getVirtualItems().map((virtualRow) => (
                  <div
                    key={virtualRow.key}
                    data-index={virtualRow.index}
                    ref={virtualizer.measureElement}
                    style={{
                      position: 'absolute',
                      top: `${virtualRow.start}px`,
                      left: 0,
                      width: '100%',
                    }}
                  >
                    <HistoryLogRow
                      row={renderRows[virtualRow.index]}
                      columns={columns}
                      searchTerm={
                        !fulltextDisabled && debouncedText
                          ? debouncedText
                          : undefined
                      }
                    />
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
    </TooltipProvider>
  )
}

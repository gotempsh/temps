'use client'

import { ContainerInfoResponse, ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { cn } from '@/lib/utils'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useVirtualizer } from '@tanstack/react-virtual'
import AnsiToHtml from 'ansi-to-html'
import {
  AlertCircle,
  ChevronDown,
  ChevronUp,
  Columns3,
  Pause,
  Play,
  RefreshCw,
  Search,
  Timer,
} from 'lucide-react'
import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { FilterBar } from './filter-bar'

// History-viewer-style row primitives. Duplicated locally (rather than
// imported from useLogStream / history-log-viewer) because this viewer holds
// `logs` as a plain string[] and parses on render — re-typing the rope as
// LiveLogLine[] would ripple through the WS callback, the rAF flusher, the
// interval poll, and tail/length math. Keeping the parse at render isolates
// the visual change to the row itself.
type LiveLogLevel = 'ERROR' | 'WARN' | 'INFO' | 'DEBUG' | 'TRACE'

const LEVEL_OPTIONS: LiveLogLevel[] = ['ERROR', 'WARN', 'INFO', 'DEBUG', 'TRACE']

const LEVEL_COLORS: Record<LiveLogLevel, string> = {
  ERROR: 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/20',
  WARN: 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 border-yellow-500/20',
  INFO: 'bg-blue-500/15 text-blue-700 dark:text-blue-400 border-blue-500/20',
  DEBUG: 'bg-zinc-500/15 text-zinc-700 dark:text-zinc-400 border-zinc-500/20',
  TRACE: 'bg-zinc-400/15 text-zinc-500 dark:text-zinc-500 border-zinc-400/20',
}

// Leading ISO timestamp the server prepends when ?timestamps=true is on.
// Docker emits RFC 3339 with nano precision (`2025-05-30T10:40:00.123456789Z`).
const TIMESTAMP_PREFIX = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)\s+/

// Severity inference — see useLogStream.ts for the matching version. Scans
// only the first ~120 chars so a stack trace containing the word "warning"
// downstream doesn't re-classify the whole row.
const LEVEL_PATTERNS: Array<[LiveLogLevel, RegExp]> = [
  ['ERROR', /\b(ERROR|ERR|FATAL|PANIC|EMERG|CRIT)\b|\bpanic:|\bfatal:/i],
  ['WARN', /\b(WARN|WARNING)\b/i],
  ['DEBUG', /\b(DEBUG|DBG)\b/i],
  ['TRACE', /\b(TRACE|TRC)\b/i],
  ['INFO', /\b(INFO|NOTICE)\b/i],
]

function inferLevel(message: string): LiveLogLevel {
  const head = message.slice(0, 120)
  for (const [level, pattern] of LEVEL_PATTERNS) {
    if (pattern.test(head)) return level
  }
  return 'INFO'
}

interface ParsedLogLine {
  level: LiveLogLevel
  timestamp?: string
  message: string
}

function parseLogLine(raw: string): ParsedLogLine {
  const tsMatch = raw.match(TIMESTAMP_PREFIX)
  const timestamp = tsMatch?.[1]
  const message = timestamp ? raw.slice(tsMatch![0].length) : raw
  return { level: inferLevel(message), timestamp, message }
}

function formatTimestamp(iso?: string): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const base = d.toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
  return `${base}.${String(d.getMilliseconds()).padStart(3, '0')}`
}

interface ColumnVisibility {
  timestamp: boolean
  level: boolean
  service: boolean
}

const COLUMNS_STORAGE_KEY = 'temps.runtime-log.columns'

// Default to the same dense terminal look as the history viewer: all three
// columns on. Persists per-browser so users who want a tighter rope can
// hide what they don't need.
const DEFAULT_COLUMNS: ColumnVisibility = {
  timestamp: true,
  level: true,
  service: true,
}

function loadColumns(): ColumnVisibility {
  if (typeof window === 'undefined') return DEFAULT_COLUMNS
  try {
    const raw = window.localStorage.getItem(COLUMNS_STORAGE_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<ColumnVisibility>
      return {
        timestamp: parsed.timestamp ?? DEFAULT_COLUMNS.timestamp,
        level: parsed.level ?? DEFAULT_COLUMNS.level,
        service: parsed.service ?? DEFAULT_COLUMNS.service,
      }
    }
  } catch {
    // Ignore corrupted storage and fall through to defaults.
  }
  return DEFAULT_COLUMNS
}

const ansiConverter = new AnsiToHtml({
  fg: 'var(--foreground)',
  bg: 'var(--background)',
  newline: false,
  escapeXML: true,
})

interface LiveLogRowProps {
  raw: string
  columns: ColumnVisibility
  searchTerm: string
  isHighlighted: boolean
  serviceLabel?: string | null
}

const LiveLogRow = memo(function LiveLogRow({
  raw,
  columns,
  searchTerm,
  isHighlighted,
  serviceLabel,
}: LiveLogRowProps) {
  const parsed = useMemo(() => parseLogLine(raw), [raw])

  // ANSI conversion is unconditional — container stdout may carry escape
  // sequences. ansi-to-html escapes XML so log content can't inject markup.
  // Search highlight is layered as a regex replace on the resulting HTML,
  // done in plain text so it doesn't mangle ANSI span boundaries.
  const messageHtml = useMemo(() => {
    const html = ansiConverter.toHtml(parsed.message)
    if (!searchTerm) return html
    const escaped = searchTerm.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    return html.replace(
      new RegExp(`(${escaped})`, 'gi'),
      '<mark class="bg-yellow-200 dark:bg-yellow-800 rounded px-1">$1</mark>',
    )
  }, [parsed.message, searchTerm])

  return (
    <div
      className={cn(
        'flex items-start gap-2 py-0.5 px-2 font-mono text-xs select-text hover:bg-muted/50',
        isHighlighted && 'bg-accent',
      )}
    >
      {columns.timestamp && (
        <span className="text-muted-foreground shrink-0 tabular-nums w-[85px]">
          {formatTimestamp(parsed.timestamp)}
        </span>
      )}
      {columns.level && (
        <Badge
          variant="outline"
          className={cn(
            'shrink-0 text-[10px] font-medium px-1.5 py-0 h-[18px] leading-[18px] rounded-sm',
            LEVEL_COLORS[parsed.level],
          )}
        >
          {parsed.level}
        </Badge>
      )}
      {columns.service && serviceLabel && (
        <span
          className="text-muted-foreground shrink-0 w-[70px] truncate"
          title={serviceLabel}
        >
          {serviceLabel}
        </span>
      )}
      <span
        className="whitespace-pre-wrap break-all min-w-0 flex-1"
        dangerouslySetInnerHTML={{ __html: messageHtml }}
      />
    </div>
  )
})

function SelectedContainerLabel({
  container,
  containerId,
}: {
  container: ContainerInfoResponse | undefined
  containerId: string
}) {
  return (
    <div className="flex items-center gap-2 text-left">
      <span className="truncate">{container?.container_name}</span>
      {container?.node_name && (
        <span className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded shrink-0">
          {container.node_name}
        </span>
      )}
      <span className="text-xs text-muted-foreground shrink-0">
        {containerId.substring(0, 12)}
      </span>
    </div>
  )
}

// Visible log cap — same in every mode. The log viewer is a peephole, not a
// recorder; if you want history, that's the History tab's job. Live/Pause/
// Interval all just decide *when* you see the most recent N lines.
const MAX_VISIBLE_LOGS = 5000
// Cap on the pending buffer between flushes. On a 1500-line/sec firehose at
// a 30s interval that's ~44k lines a tick — we don't want 44k strings sitting
// in JS memory waiting to render. Trim to the most recent 1000 and surface
// the dropped count so the user knows they're sampling, not recording.
const MAX_PENDING_BUFFER = 1000
// Minimum gap between Live-mode flushes. The previous behavior scheduled
// a rAF on every WS frame; on a 1000+ lps firehose the eye perceives a
// blurred wall of replacing text. 33ms (~30Hz) gives the user a perceivable
// batch cadence — fast enough to feel live, slow enough that any one batch
// is visually distinct from the next.
const LIVE_FLUSH_MIN_GAP_MS = 33
// Window over which we compute the live lines-per-second readout. 5s is
// short enough to react to bursts but long enough that brief lulls don't
// flap the displayed number.
const LPS_WINDOW_MS = 5_000
// Threshold above which we surface a `sampled` chip — once incoming exceeds
// this, the visible buffer cap will roll lines off faster than the user can
// read, so they should know they're not seeing every line.
const LPS_SAMPLED_THRESHOLD = 60

const INTERVAL_OPTIONS_MS = [5_000, 30_000, 60_000] as const
type IntervalMs = (typeof INTERVAL_OPTIONS_MS)[number]

type LogMode =
  | { kind: 'live' }
  | { kind: 'pause' }
  | { kind: 'interval'; ms: IntervalMs }

const DEFAULT_MODE: LogMode = { kind: 'live' }
const DEFAULT_INTERVAL_MS: IntervalMs = 5_000

function modeStorageKey(projectSlug: string) {
  return `temps:runtime-logs:mode:${projectSlug}`
}

function loadPersistedMode(projectSlug: string): LogMode {
  if (typeof window === 'undefined') return DEFAULT_MODE
  try {
    const raw = window.localStorage.getItem(modeStorageKey(projectSlug))
    if (!raw) return DEFAULT_MODE
    const parsed = JSON.parse(raw) as LogMode
    if (parsed.kind === 'live' || parsed.kind === 'pause') return parsed
    if (
      parsed.kind === 'interval' &&
      (INTERVAL_OPTIONS_MS as readonly number[]).includes(parsed.ms)
    ) {
      return parsed
    }
  } catch {
    // ignore — corrupt entry
  }
  return DEFAULT_MODE
}

function persistMode(projectSlug: string, mode: LogMode) {
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(modeStorageKey(projectSlug), JSON.stringify(mode))
  } catch {
    // ignore — quota / disabled storage
  }
}

function formatIntervalLabel(ms: IntervalMs): string {
  if (ms < 60_000) return `${Math.round(ms / 1000)}s`
  return `${Math.round(ms / 60_000)}m`
}

// Row height is fixed at 22px (see virtualizer config). Per-row measurement
// previously lived here as estimateLineHeight; removed when we adopted the
// history viewer's terminal-style fixed-cadence rows.

export default function LogViewer({ project }: { project: ProjectResponse }) {
  const [logs, setLogs] = useState<string[]>([])
  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'connected' | 'error' | 'permanent_error'
  >('connecting')
  const [retryCount, setRetryCount] = useState(0)
  const [errorMessage, setErrorMessage] = useState('')
  const [searchTerm, setSearchTerm] = useState('')
  const [currentMatchIndex, setCurrentMatchIndex] = useState(-1)
  const [startDate, setStartDate] = useState<Date>()
  const [endDate, setEndDate] = useState<Date>()
  const [selectedTarget, setSelectedTarget] = useState<number>()
  const [selectedContainer, setSelectedContainer] = useState<string>()
  const [showAdvanced, setShowAdvanced] = useState(false)
  const [tail, setTail] = useState<number>(1000)
  const [autoScroll, setAutoScroll] = useState(true)
  const [showTimestamps, setShowTimestamps] = useState(false)
  // Level chip filter — empty array means "show all", matching the history
  // viewer. Levels are inferred per-line at render via parseLogLine; the
  // filtered rope below memoizes that so the virtualizer only sees survivors.
  const [selectedLevels, setSelectedLevels] = useState<LiveLogLevel[]>([])
  const [columns, setColumns] = useState<ColumnVisibility>(loadColumns)
  useEffect(() => {
    try {
      window.localStorage.setItem(COLUMNS_STORAGE_KEY, JSON.stringify(columns))
    } catch {
      // Storage may be unavailable (private mode, quota); not worth surfacing.
    }
  }, [columns])
  // Turning on the Timestamp column implies asking the server for timestamps.
  // Without this, the column slot would render empty for every row. The WS
  // effect already restarts on showTimestamps change.
  useEffect(() => {
    if (columns.timestamp && !showTimestamps) {
      setShowTimestamps(true)
    }
  }, [columns.timestamp, showTimestamps])
  // Refresh mode: Live (rAF every frame), Pause (never auto-flush), or
  // Interval (flush every N ms). Persisted per-project so users don't re-pick
  // it every visit.
  const [mode, setMode] = useState<LogMode>(() => loadPersistedMode(project.slug))
  // Last interval the user picked, so toggling "Interval" remembers their
  // previous duration instead of resetting to 5s every time.
  const [lastIntervalMs, setLastIntervalMs] = useState<IntervalMs>(() => {
    const persisted = loadPersistedMode(project.slug)
    return persisted.kind === 'interval' ? persisted.ms : DEFAULT_INTERVAL_MS
  })
  // Visible counter: how many lines are sitting in the pending buffer waiting
  // for the next flush. We re-render this on a slow cadence so the counter
  // doesn't cost per-line renders.
  const [bufferedCount, setBufferedCount] = useState(0)
  // Lines that arrived while the buffer was already full — never made it into
  // React state. Surfaced in the status row so users notice they're sampling
  // and can switch to a shorter interval, Live mode, or the History tab.
  const [droppedSinceFlush, setDroppedSinceFlush] = useState(0)
  // Ref mirror so the WS callback (created once) can increment without
  // depending on the latest setter closure.
  const droppedSinceFlushRef = useRef(0)
  // Countdown-to-next-tick state, only meaningful in Interval mode.
  const [nextTickAt, setNextTickAt] = useState<number | null>(null)
  const [now, setNow] = useState(() => Date.now())
  const parentRef = useRef<HTMLDivElement>(null)
  const wsRef = useRef<WebSocket | null>(null)
  const isConnectingRef = useRef(false)
  const retryTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  // Buffered lines awaiting flush. Drained at different cadences depending on
  // mode (rAF for live, setInterval for interval, never for pause).
  const pendingLogsRef = useRef<string[]>([])
  const rafHandleRef = useRef<number | null>(null)
  const flushTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const lastFlushTsRef = useRef<number>(0)
  const intervalHandleRef = useRef<ReturnType<typeof setInterval> | null>(null)
  // Sliding window of WS message arrival timestamps used to compute live
  // lines-per-second. Pushed on every enqueueLog; trimmed on demand. Stays
  // a ref (not state) because we re-read the count once per second on the
  // existing setNow tick — no need to render on every push.
  const lpsSamplesRef = useRef<number[]>([])
  const [lps, setLps] = useState(0)
  // Index of the first row in the most recently flushed batch, captured at
  // flush time. Rendered rows whose virtual index is >= this get a fade-in
  // animation so the user perceives the new batch arriving as motion rather
  // than a wall expanding silently.
  const lastBatchStartRef = useRef<number>(0)
  const [lastBatchStart, setLastBatchStart] = useState(0)
  // Set by the Interval polling effect to its current poll function. The
  // "Refresh now" button calls this to fire an immediate poll instead of
  // waiting for the next tick. Null when not in Interval mode.
  const pollNowRef = useRef<(() => void) | null>(null)
  // Live-mirror of mode/lastInterval so the WS callbacks (created once) can
  // read the latest value without being re-bound on every change.
  const modeRef = useRef<LogMode>(mode)
  // Signature of the WS effect's *source* params (everything except mode).
  // If the new run has the same signature, we're toggling modes and must
  // NOT wipe the visible logs — otherwise switching Live → Interval blanks
  // the pane until the next tick (5s of empty screen). Source-param
  // changes (env, container, dates, tail, timestamps) still wipe.
  const sourceSigRef = useRef<string>('')

  // Filtered rope — drops lines whose inferred level isn't in the active
  // selection. Search filtering stays at the row level (the LiveLogRow
  // highlights matches) because filtering on the search term would hide the
  // surrounding context that makes a match useful to read. The level chips
  // are a real reducer: they collapse the visible rope so the count, the
  // virtualizer, and the search-nav all agree on what's on screen.
  const filteredLogs = useMemo(() => {
    if (selectedLevels.length === 0) return logs
    return logs.filter((raw) => selectedLevels.includes(inferLevel(raw)))
  }, [logs, selectedLevels])

  // Dynamic row height. Log lines are often long JSON payloads that wrap to
  // several visual lines; a fixed 22px row caused wrapped content to overflow
  // its slot and render on top of the rows below. `measureElement` reports each
  // row's real height to the virtualizer (estimateSize is just the initial
  // guess for off-screen rows). The row wrapper sets `ref` + `data-index` so
  // the virtualizer can measure it.
  const virtualizer = useVirtualizer({
    count: filteredLogs.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 22,
    measureElement: (el) => el.getBoundingClientRect().height,
    overscan: 20,
  })
  // Poll the environments list. `current_deployment_id` on the entry whose
  // id == selectedTarget is the per-environment "what's running right now"
  // pointer; we watch it below to detect redeploys for the env the user
  // is actually looking at. Polling here is fine because it's a single
  // small query and avoids a separate per-env endpoint.
  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    refetchInterval: 3000,
    refetchOnWindowFocus: true,
  })

  const queryClient = useQueryClient()

  // Fetch containers for the selected environment. The list itself doesn't
  // need polling — instead we watch the project's last-deployment id below
  // and invalidate this query when it flips, which is a far cheaper and
  // more deterministic signal than blind polling.
  const containersQueryKey = useMemo(
    () =>
      listContainersOptions({
        path: {
          project_id: project.id,
          environment_id: selectedTarget || 0,
        },
      }).queryKey,
    [project.id, selectedTarget],
  )

  const { data: containersData } = useQuery({
    ...listContainersOptions({
      path: {
        project_id: project.id,
        environment_id: selectedTarget || 0,
      },
    }),
    enabled: !!selectedTarget,
    staleTime: 0,
    gcTime: 0,
    refetchOnMount: 'always',
    refetchOnWindowFocus: true,
  })

  // The per-environment `current_deployment_id` is the canonical "what is
  // running right now" pointer. Watching this (rather than the project-wide
  // last deployment) means a redeploy in *another* environment won't make
  // us drop logs we're tailing here.
  const currentEnv = environments?.find((e) => e.id === selectedTarget)
  const currentDeploymentId = currentEnv?.current_deployment_id ?? null

  // When the env's current deployment id flips, refresh the container list
  // so the reconciliation effect can pick up the new containers. Crucially
  // we DO NOT clear `selectedContainer` here — that creates a window where
  // the WS effect bails out (no container) and then races with the new
  // list arriving. Instead we let the reconciliation effect atomically
  // swap selectedContainer once the new container is visible in the list,
  // which keeps the WS lifecycle to a single clean reconnect.
  const previousDeploymentIdRef = useRef<number | null>(null)
  useEffect(() => {
    if (currentDeploymentId == null) return
    const prev = previousDeploymentIdRef.current
    previousDeploymentIdRef.current = currentDeploymentId
    if (prev != null && prev !== currentDeploymentId) {
      queryClient.invalidateQueries({ queryKey: containersQueryKey })
    }
  }, [currentDeploymentId, queryClient, containersQueryKey])

  // Auto-select first environment when environments are loaded
  useEffect(() => {
    if (environments && environments.length > 0 && !selectedTarget) {
      setSelectedTarget(environments[0].id)
    }
  }, [environments, selectedTarget])

  // Reconcile selectedContainer with the latest container list:
  //   - if nothing is selected, pick the first container
  //   - if the previously-selected container is no longer present (e.g. it
  //     was destroyed by a redeploy), fall back to the first available
  // Only acts when the list has at least one container, so a transient
  // empty-list response during the redeploy window won't yank an
  // already-good selection out from under the WS.
  useEffect(() => {
    const containers = containersData?.containers ?? []
    if (containers.length === 0) return

    const stillExists = containers.some(
      (c) => c.container_id === selectedContainer,
    )
    if (!selectedContainer || !stillExists) {
      setSelectedContainer(containers[0].container_id)
    }
  }, [containersData, selectedContainer])

  // Service label for the Service column. Falls back to the container name
  // when a service isn't named explicitly (e.g. ad-hoc deploys). Memoized so
  // every LiveLogRow render doesn't trigger a fresh array scan.
  const selectedContainerServiceName = useMemo(() => {
    if (!selectedContainer) return null
    const container = containersData?.containers?.find(
      (c) => c.container_id === selectedContainer,
    )
    return container?.service_name ?? container?.container_name ?? null
  }, [containersData, selectedContainer])

  const toggleLevel = useCallback((level: LiveLogLevel) => {
    setSelectedLevels((prev) =>
      prev.includes(level) ? prev.filter((l) => l !== level) : [...prev, level],
    )
  }, [])

  // Persist mode + keep a ref in sync for WS callbacks.
  useEffect(() => {
    modeRef.current = mode
    persistMode(project.slug, mode)
  }, [mode, project.slug])

  // Drain pendingLogsRef into the visible logs list, then trim to the visible
  // cap. Same cap in every mode — the buffer is a peephole, not a recorder.
  const flushPending = useCallback(() => {
    const incoming = pendingLogsRef.current
    pendingLogsRef.current = []
    setBufferedCount(0)
    setDroppedSinceFlush(0)
    droppedSinceFlushRef.current = 0
    if (incoming.length === 0) return
    lastFlushTsRef.current = Date.now()
    setLogs((prev) => {
      const merged = [...prev, ...incoming]
      const trimmed =
        merged.length > MAX_VISIBLE_LOGS
          ? merged.slice(-MAX_VISIBLE_LOGS)
          : merged
      // Capture where this batch starts in the trimmed list so the row
      // renderer can apply the fade-in animation to just the new rows.
      // When we trim, the start is `length - incoming.length`; when we
      // don't, it's `prev.length`.
      const batchStart = Math.max(0, trimmed.length - incoming.length)
      lastBatchStartRef.current = batchStart
      setLastBatchStart(batchStart)
      return trimmed
    })
  }, [])

  // Enqueue a single line. In Live mode flushes are throttled to at most one
  // every LIVE_FLUSH_MIN_GAP_MS so the visible cadence stays eye-paceable
  // even on a firehose stream. In Pause/Interval modes the line just sits in
  // the buffer until the corresponding mechanism drains it. The buffer is
  // capped at MAX_PENDING_BUFFER so a high-volume stream during a 30s tick
  // can't accumulate megabytes in JS memory. When the cap is hit we drop
  // the oldest pending line and bump the dropped counter so the user
  // notices they're sampling.
  const enqueueLog = useCallback(
    (line: string) => {
      const buf = pendingLogsRef.current
      buf.push(line)
      if (buf.length > MAX_PENDING_BUFFER) {
        const overflow = buf.length - MAX_PENDING_BUFFER
        buf.splice(0, overflow)
        droppedSinceFlushRef.current += overflow
      }
      // Track WS arrival time for the lps readout. Trimmed lazily on the
      // 1Hz tick effect — pushing here is O(1).
      lpsSamplesRef.current.push(Date.now())

      if (modeRef.current.kind === 'live') {
        // Already a flush scheduled — let it pick up this line.
        if (rafHandleRef.current != null || flushTimeoutRef.current != null)
          return

        const now = Date.now()
        const sinceLast = now - lastFlushTsRef.current
        if (sinceLast >= LIVE_FLUSH_MIN_GAP_MS) {
          // Past the gap — flush on the next frame as before.
          rafHandleRef.current = requestAnimationFrame(() => {
            rafHandleRef.current = null
            flushPending()
          })
        } else {
          // Within the gap — defer the flush until the gap elapses, then
          // run on the following animation frame so the DOM mutation
          // aligns with paint timing.
          const wait = LIVE_FLUSH_MIN_GAP_MS - sinceLast
          flushTimeoutRef.current = setTimeout(() => {
            flushTimeoutRef.current = null
            rafHandleRef.current = requestAnimationFrame(() => {
              rafHandleRef.current = null
              flushPending()
            })
          }, wait)
        }
      } else {
        // For Pause/Interval, keep counters fresh on a slow cadence — the
        // dedicated effect below ticks `now` once a second.
        setBufferedCount(buf.length)
        if (droppedSinceFlushRef.current > 0) {
          setDroppedSinceFlush(droppedSinceFlushRef.current)
        }
      }
    },
    [flushPending],
  )

  // Drive the Interval mode poll + countdown. Also drives the 1Hz "now" tick
  // so the "Next refresh in 0:03" / lps UIs stay live.
  //
  // Interval mode does NOT use the WebSocket. Instead it polls the
  // /api/logs/search endpoint every N seconds and replaces the visible
  // buffer with the most recent 500 lines. Snapshot semantics — what you
  // see is "the last 500 lines as of N seconds ago," well-defined and
  // free of duplicate bursts that an interval-flushed-WS produced.
  useEffect(() => {
    const nowTimer = setInterval(() => {
      const t = Date.now()
      setNow(t)
      const cutoff = t - LPS_WINDOW_MS
      const samples = lpsSamplesRef.current
      let firstFresh = 0
      while (firstFresh < samples.length && samples[firstFresh] < cutoff) {
        firstFresh++
      }
      if (firstFresh > 0) samples.splice(0, firstFresh)
      const windowSec = LPS_WINDOW_MS / 1000
      setLps(samples.length / windowSec)
    }, 1000)
    if (mode.kind !== 'interval') {
      setNextTickAt(null)
      return () => clearInterval(nowTimer)
    }
    if (!selectedTarget) {
      return () => clearInterval(nowTimer)
    }
    const ms = mode.ms
    // Abort signal so any in-flight fetch when mode/source changes mid-poll
    // is cancelled — prevents a slow response from clobbering the buffer
    // after the user has switched away.
    const abort = new AbortController()
    const poll = async () => {
      try {
        const now = new Date()
        const start = new Date(now.getTime() - 60 * 60 * 1000) // last 1h
        const body: Record<string, unknown> = {
          project_id: project.id,
          start_time: start.toISOString(),
          end_time: now.toISOString(),
          envs: [String(selectedTarget)],
          page_size: 500,
        }
        const res = await fetch('/api/logs/search', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          credentials: 'include',
          body: JSON.stringify(body),
          signal: abort.signal,
        })
        if (!res.ok) return
        const json = (await res.json()) as {
          lines: Array<{
            timestamp: string
            level: string
            service: string
            message: string
          }>
        }
        // Lines arrive ASC (oldest first → newest last), matching the
        // viewer's existing "newest at the bottom" assumption. We render
        // a single-line representation so the new format slots into the
        // existing string-based logs[] buffer without changes.
        const formatted = json.lines.map((l) => {
          if (showTimestamps) {
            return `${l.timestamp} [${l.level}] ${l.service}: ${l.message}`
          }
          return `[${l.level}] ${l.service}: ${l.message}`
        })
        if (!abort.signal.aborted) {
          setLogs(formatted)
          // Captured-batch animation: treat the whole snapshot as a fresh
          // batch so newly-rendered rows fade in.
          lastBatchStartRef.current = 0
          setLastBatchStart(0)
        }
      } catch {
        // Ignore network errors; the next tick will retry.
      }
    }
    // Expose the poll function so "Refresh now" can fire an immediate poll
    // and reset the countdown without waiting for setInterval's next tick.
    pollNowRef.current = () => {
      void poll()
      setNextTickAt(Date.now() + ms)
    }
    // Fire immediately so the user sees logs on entry instead of staring
    // at a blank pane until the first tick elapses.
    void poll()
    setNextTickAt(Date.now() + ms)
    intervalHandleRef.current = setInterval(() => {
      void poll()
      setNextTickAt(Date.now() + ms)
    }, ms)
    return () => {
      clearInterval(nowTimer)
      abort.abort()
      pollNowRef.current = null
      if (intervalHandleRef.current != null) {
        clearInterval(intervalHandleRef.current)
        intervalHandleRef.current = null
      }
    }
  }, [mode, project.id, selectedTarget, showTimestamps])

  // When entering Pause from Live, cancel any pending rAF AND any deferred
  // throttle timeout so neither fires after the user just paused. The
  // buffered counter immediately reflects whatever was already pending.
  useEffect(() => {
    if (mode.kind === 'live') return
    if (rafHandleRef.current != null) {
      cancelAnimationFrame(rafHandleRef.current)
      rafHandleRef.current = null
    }
    if (flushTimeoutRef.current != null) {
      clearTimeout(flushTimeoutRef.current)
      flushTimeoutRef.current = null
    }
    setBufferedCount(pendingLogsRef.current.length)
  }, [mode])

  // Manual "Refresh now" — Interval mode triggers an immediate poll via the
  // ref the Interval effect populated. Other modes drain the rAF buffer.
  const flushNow = useCallback(() => {
    if (modeRef.current.kind === 'interval' && pollNowRef.current) {
      pollNowRef.current()
      return
    }
    flushPending()
  }, [flushPending])

  // WebSocket connection effect
  useEffect(() => {
    if (!selectedTarget) return

    // Wait for container to be selected - don't connect without a specific container
    if (!selectedContainer) return

    // Build a source-only signature (everything that affects what stream we
     // connect to, excluding mode). If only mode changed since the last run,
     // we're toggling Live/Pause/Interval and must keep the visible logs.
    const sourceSig = JSON.stringify({
      p: project.id,
      e: selectedTarget,
      c: selectedContainer,
      sd: startDate?.getTime() ?? null,
      ed: endDate?.getTime() ?? null,
      t: tail,
      ts: showTimestamps,
    })
    const sourceUnchanged = sourceSigRef.current === sourceSig
    sourceSigRef.current = sourceSig

    // Pause mode: don't open the WS at all. The cleanup from the previous
    // effect run (if any) has already closed any existing socket. We keep the
    // visible logs intact so the user can read what's on screen.
    if (mode.kind === 'pause') {
      return
    }

    // Interval mode: don't open the WS either. A separate polling effect
    // calls /api/logs/search every N seconds and replaces the visible buffer
    // with a fresh snapshot. Streaming + interval-replace is a worse UX
    // (duplicate bursts on every interval reconnect) and wastes a connection.
    if (mode.kind === 'interval') {
      return
    }

    // Capture the container this effect-instance is tailing. Used by the
    // socket handlers below to reject any late frames from a previous
    // socket whose handlers may still fire while React is unwinding.
    const targetContainer = selectedContainer
    // Only wipe state when it's a genuine source change. Mode toggles
    // (Live → Interval, Pause → Live, etc.) keep the visible buffer so the
    // user doesn't stare at a blank pane while waiting for the next tick.
    if (!sourceUnchanged) {
      setLogs([])
      pendingLogsRef.current = []
      setBufferedCount(0)
      setDroppedSinceFlush(0)
      droppedSinceFlushRef.current = 0
    }
    if (rafHandleRef.current != null) {
      cancelAnimationFrame(rafHandleRef.current)
      rafHandleRef.current = null
    }
    if (flushTimeoutRef.current != null) {
      clearTimeout(flushTimeoutRef.current)
      flushTimeoutRef.current = null
    }
    setRetryCount(0)
    setErrorMessage('')
    isConnectingRef.current = false

    let isCleaningUp = false
    let currentRetryCount = 0

    const connectWS = () => {
      if (isCleaningUp) {
        return
      }

      isConnectingRef.current = true
      const params = new URLSearchParams()
      if (startDate) {
        params.append(
          'start_date',
          Math.floor(startDate.getTime() / 1000).toString()
        )
      }
      if (endDate) {
        params.append(
          'end_date',
          Math.floor(endDate.getTime() / 1000).toString()
        )
      }
      // When the source params didn't change (we're just toggling modes),
      // ask for a small backlog so the user sees a smooth catch-up rather
      // than a wall of history dumped on top of what's already on screen.
      // On a genuine source change use the full tail.
      const effectiveTail = sourceUnchanged ? Math.min(tail, 200) : tail
      if (effectiveTail) {
        params.append('tail', effectiveTail.toString())
      }
      // Add timestamps parameter
      params.append('timestamps', showTimestamps.toString())

      // Use container-specific endpoint (selectedContainer is guaranteed by the guard above)
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
      const wsUrl = `${protocol}//${window.location.host}/api/projects/${project.id}/environments/${selectedTarget}/containers/${targetContainer}/logs?${params.toString()}`

      // Close any prior socket and detach its handlers so its in-flight
      // frames or close events can't bleed into this connection's state.
      if (wsRef.current) {
        const prev = wsRef.current
        prev.onopen = null
        prev.onmessage = null
        prev.onerror = null
        prev.onclose = null
        try {
          prev.close(1000, 'Reconnecting')
        } catch {
          // best-effort
        }
        wsRef.current = null
      }

      try {
        const ws = new WebSocket(wsUrl)
        wsRef.current = ws
        setConnectionStatus('connecting')

        ws.onopen = () => {
          // Stale-socket guard: if React already swapped us out before the
          // open event fired, do nothing.
          if (ws !== wsRef.current || isCleaningUp) return
          setConnectionStatus('connected')
          currentRetryCount = 0
          setRetryCount(0)
          setErrorMessage('')
          isConnectingRef.current = false

          // Clear any pending retry timeouts
          if (retryTimeoutRef.current) {
            clearTimeout(retryTimeoutRef.current)
            retryTimeoutRef.current = null
          }
        }

        ws.onmessage = (event) => {
          // Drop frames from any socket that's no longer the active one —
          // prevents old-deployment "Deployment not found" frames from
          // bleeding into the freshly-connected container's log buffer.
          if (ws !== wsRef.current || isCleaningUp) return
          try {
            // Try to parse as JSON first
            const parsed = JSON.parse(event.data)

            if (parsed.error && parsed.stack) {
              enqueueLog(`ERROR: ${parsed.error}\n${parsed.stack}`)
            } else if (parsed.message) {
              enqueueLog(parsed.message)
            } else if (parsed.log) {
              enqueueLog(parsed.log)
            } else {
              enqueueLog(JSON.stringify(parsed, null, 2))
            }
          } catch {
            // If it's not JSON, just use it as-is
            enqueueLog(event.data)
          }
        }

        ws.onerror = (error) => {
          if (ws !== wsRef.current || isCleaningUp) return
          console.error('WebSocket error:', error)
          setErrorMessage('Connection failed')
          isConnectingRef.current = false
        }

        ws.onclose = (event) => {
          // Late close from a replaced socket — ignore.
          if (ws !== wsRef.current) return
          isConnectingRef.current = false

          // Don't reconnect if cleaning up or normal closure
          if (isCleaningUp || event.code === 1000) {
            return
          }

          // Increment retry count
          currentRetryCount++
          setRetryCount(currentRetryCount)

          if (currentRetryCount >= 3) {
            setConnectionStatus('permanent_error')
            setErrorMessage('Connection failed after multiple attempts')
            return
          }

          // Temporary error - attempt to reconnect
          setConnectionStatus('error')
          const delay = Math.pow(2, currentRetryCount) * 1000

          // Clear any existing retry timeout
          if (retryTimeoutRef.current) {
            clearTimeout(retryTimeoutRef.current)
          }

          retryTimeoutRef.current = setTimeout(() => {
            retryTimeoutRef.current = null
            connectWS()
          }, delay)
        }
      } catch (error) {
        console.error('Failed to create WebSocket:', error)
        setConnectionStatus('permanent_error')
        setErrorMessage('Failed to establish connection')
        isConnectingRef.current = false
      }
    }

    connectWS()

    return () => {
      isCleaningUp = true
      isConnectingRef.current = false

      // Cancel any pending log flush so it can't fire after unmount.
      if (rafHandleRef.current != null) {
        cancelAnimationFrame(rafHandleRef.current)
        rafHandleRef.current = null
      }
      if (flushTimeoutRef.current != null) {
        clearTimeout(flushTimeoutRef.current)
        flushTimeoutRef.current = null
      }
      pendingLogsRef.current = []

      // Clear any pending retry timeout
      if (retryTimeoutRef.current) {
        clearTimeout(retryTimeoutRef.current)
        retryTimeoutRef.current = null
      }

      // Detach handlers before closing so any late events from the closing
      // socket can't write to React state on the next mounted instance.
      const ws = wsRef.current
      if (ws) {
        ws.onopen = null
        ws.onmessage = null
        ws.onerror = null
        ws.onclose = null
        try {
          ws.close(1000, 'Component unmounting')
        } catch {
          // best-effort
        }
        wsRef.current = null
      }
    }
    // `containersData` is intentionally NOT in this dep array. The polling
    // refetch (every 3s) updates that object on every tick; if it were a
    // dep, we would tear down and re-open the WebSocket every 3s and the
    // user would see the "Connection lost" banner flap forever. The
    // reconciliation effect above already swaps `selectedContainer` when
    // the active container disappears, which *does* re-trigger this effect
    // via the selectedContainer dep — that's the only legitimate reason
    // to reconnect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    project.id,
    project.slug,
    selectedTarget,
    selectedContainer,
    startDate,
    endDate,
    tail,
    showTimestamps,
    // Re-run when pause is toggled so the cleanup closes / a fresh connection
    // opens. We watch mode.kind, not the full mode object, so changing the
    // interval duration in Interval mode doesn't churn the WS.
    mode.kind,
  ])

  // Shared connectWS function for retry
  const handleRetryConnection = useCallback(() => {
    setRetryCount(0)
    setConnectionStatus('connecting')
    setErrorMessage('')

    const params = new URLSearchParams()
    if (startDate) {
      params.append(
        'start_date',
        Math.floor(startDate.getTime() / 1000).toString()
      )
    }
    if (endDate) {
      params.append('end_date', Math.floor(endDate.getTime() / 1000).toString())
    }
    if (tail) {
      params.append('tail', tail.toString())
    }
    // Add timestamps parameter
    params.append('timestamps', showTimestamps.toString())

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const wsUrl = `${protocol}//${window.location.host}/api/projects/${project.id}/environments/${selectedTarget}/containers/${selectedContainer}/logs?${params.toString()}`

    // Close existing connection if any
    if (wsRef.current) {
      wsRef.current.close()
    }

    try {
      wsRef.current = new WebSocket(wsUrl)
      setConnectionStatus('connecting')

      wsRef.current.onopen = () => {
        setConnectionStatus('connected')
        setRetryCount(0)
        setErrorMessage('')
      }

      wsRef.current.onmessage = (event) => {
        try {
          const parsed = JSON.parse(event.data)
          if (parsed.error && parsed.stack) {
            enqueueLog(`ERROR: ${parsed.error}\n${parsed.stack}`)
          } else if (parsed.message) {
            enqueueLog(parsed.message)
          } else if (parsed.log) {
            enqueueLog(parsed.log)
          } else {
            enqueueLog(JSON.stringify(parsed, null, 2))
          }
        } catch {
          enqueueLog(event.data)
        }
      }

      wsRef.current.onerror = () => {
        // Try to extract more details from the error event
        let errorMessage = 'Connection failed'
        setErrorMessage(errorMessage)
        wsRef.current?.close()

        setRetryCount((prev) => {
          const newRetryCount = prev + 1
          if (newRetryCount >= 3) {
            setConnectionStatus('permanent_error')
            setErrorMessage('Connection failed after multiple attempts')
            return newRetryCount
          }

          setConnectionStatus('error')

          setTimeout(
            () => {
              if (wsRef.current !== null) {
                handleRetryConnection()
              }
            },
            Math.pow(2, newRetryCount) * 1000
          )

          return newRetryCount
        })
      }
    } catch {
      setConnectionStatus('permanent_error')
      setErrorMessage('Failed to establish connection')
    }
  }, [
    project.id,
    selectedTarget,
    selectedContainer,
    startDate,
    endDate,
    tail,
    showTimestamps,
    enqueueLog,
  ])

  // Reset the active match index when the search term changes. We
  // deliberately do NOT scan the DOM for matches (the previous implementation
  // ran `document.querySelectorAll('[id^="search-match-"]')` on every new log
  // line — those ids are never emitted by `LogLine`, so the scan was both
  // dead code and an O(N) DOM walk per WS frame, which is what crashed the
  // page on high-volume streams).
  useEffect(() => {
    setCurrentMatchIndex(-1)
  }, [searchTerm])

  // Auto-scroll-to-bottom when in follow mode. Two subtleties beyond a naive
  // `scrollTop = scrollHeight`:
  //
  // 1. tanstack-virtual measures rows lazily as they scroll into view; a
  //    multi-line JSON log can grow by 60+ pixels post-measurement. If we
  //    only set scrollTop once on the `logs` change, the virtualizer's
  //    follow-up reflow pushes us above the true bottom and the next
  //    handleScroll incorrectly disengages follow mode. Schedule the snap
  //    on rAF so it runs after the virtualizer commit.
  //
  // 2. Don't pin if we're already at the bottom — avoids cancelling
  //    momentum-scroll on macOS when a flick happens to land at-bottom.
  useEffect(() => {
    if (!autoScroll) return
    const root = parentRef.current
    if (!root) return
    const id = requestAnimationFrame(() => {
      // Re-check inside the rAF: the user may have scrolled away in the
      // intervening tick, in which case we must not yank them back.
      if (!autoScroll) return
      const target = root.scrollHeight - root.clientHeight
      if (root.scrollTop < target) {
        root.scrollTop = target
      }
    })
    return () => cancelAnimationFrame(id)
  }, [logs, autoScroll])

  // Track whether the most recent scroll event was user-driven. handleScroll
  // alone can't tell — content reflow inside the virtualizer dispatches the
  // same event shape. We mark `userScrolledRef` on wheel / keydown / touch
  // and only let those events disengage follow mode. Re-engagement (from
  // false → true) still works on any scroll-to-bottom because that's the
  // explicit signal "I want to follow again".
  const userScrolledRef = useRef(false)
  const markUserIntent = useCallback(() => {
    userScrolledRef.current = true
  }, [])

  const handleScroll = (event: React.UIEvent<HTMLDivElement>) => {
    const { scrollTop, scrollHeight, clientHeight } = event.currentTarget
    // Tolerance of 8px absorbs sub-pixel rounding plus the ~1–4px gap that
    // the virtualizer's post-measurement reflow can leave behind.
    const isAtBottom = scrollHeight - scrollTop - clientHeight < 8
    if (isAtBottom) {
      // User (or our own snap) is at the bottom — engage follow.
      if (!autoScroll) setAutoScroll(true)
      userScrolledRef.current = false
      return
    }
    // Not at the bottom. Only disengage if the user actually moved us here —
    // otherwise this is a reflow-induced scroll event and we ignore it.
    if (userScrolledRef.current && autoScroll) {
      setAutoScroll(false)
    }
  }

  const handleSearch = useCallback((value: string) => {
    setSearchTerm(value)
    setCurrentMatchIndex(0)
  }, [])

  const handleRetry = () => {
    handleRetryConnection()
  }

  return (
    <div className="w-full">
      <div className="rounded-lg border bg-background shadow-sm">
        {/* Connection status alerts — only meaningful in Live mode where we
            actually hold a WebSocket. Pause has no socket; Interval polls
            HTTP and surfaces errors silently (the next tick retries). */}
        {mode.kind === 'live' && connectionStatus === 'connecting' && (
          <Alert className="m-4">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>Connecting to log stream...</AlertDescription>
          </Alert>
        )}

        {mode.kind === 'live' && connectionStatus === 'error' && (
          <Alert variant="destructive" className="m-4">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Connection lost. Attempting to reconnect... (Attempt {retryCount}
              /3)
            </AlertDescription>
          </Alert>
        )}

        {mode.kind === 'live' && connectionStatus === 'permanent_error' && (
          <Alert variant="destructive" className="m-4">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription className="flex items-center justify-between">
              <span>{errorMessage || 'Connection failed permanently'}</span>
              <Button
                variant="outline"
                size="sm"
                onClick={handleRetry}
                className="ml-4"
              >
                Retry Connection
              </Button>
            </AlertDescription>
          </Alert>
        )}

        {/* Main Filters */}
        <div className="px-3 py-2 space-y-2">
          <div className="flex flex-col sm:flex-row gap-2">
            <Select
              value={selectedTarget?.toString()}
              onValueChange={(value) => {
                setSelectedTarget(Number(value))
                setSelectedContainer(undefined)
              }}
            >
              <SelectTrigger className="w-full sm:w-[250px]">
                <SelectValue placeholder="Select environment" />
              </SelectTrigger>
              <SelectContent>
                {environments?.map((environment) => (
                  <SelectItem
                    key={environment.id}
                    value={environment.id.toString()}
                  >
                    {environment.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            <Select
              value={selectedContainer}
              onValueChange={(value) => setSelectedContainer(value)}
            >
              <SelectTrigger className="w-full sm:w-auto sm:max-w-[400px] text-left">
                <SelectValue placeholder="Select container">
                  {selectedContainer && (
                    <SelectedContainerLabel
                      container={containersData?.containers?.find(
                        (x) => x.container_id === selectedContainer
                      )}
                      containerId={selectedContainer}
                    />
                  )}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {containersData?.containers?.map((container) => (
                  <SelectItem
                    key={container.container_id}
                    value={container.container_id}
                  >
                    <div className="flex flex-col items-start text-left">
                      <div className="flex items-center gap-2">
                        <span>{container.container_name}</span>
                        {container.node_name && (
                          <span className="text-xs text-muted-foreground bg-muted px-1.5 py-0.5 rounded">
                            {container.node_name}
                          </span>
                        )}
                      </div>
                      <span className="text-xs text-muted-foreground">
                        {container.container_id.substring(0, 12)}
                      </span>
                    </div>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            <div className="relative flex-1">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
              <Input
                placeholder="Search logs..."
                value={searchTerm}
                onChange={(e) => handleSearch(e.target.value)}
                className="pl-9 w-full"
              />
            </div>

            {/* Columns dropdown — same set + storage key spirit as the
                container logs viewer and the history viewer. Service column
                is disabled when no container is selected so the slot isn't
                advertised when there's nothing to put in it. */}
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-9 gap-1.5"
                  aria-label="Toggle columns"
                >
                  <Columns3 className="h-3.5 w-3.5" />
                  <span className="hidden sm:inline text-xs">Columns</span>
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
                  disabled={!selectedContainerServiceName}
                >
                  Service
                </DropdownMenuCheckboxItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>

          {/* Level chip row — mirrors history-log-viewer.tsx. Empty selection
              = show all; once a level is picked, only matching rows survive
              the filteredLogs memo above. Layout intentionally lives between
              the source-picker row and the mode-segmented control so it's
              always visible regardless of Advanced Options state. */}
          <div className="flex gap-1.5 flex-wrap items-center">
            {LEVEL_OPTIONS.map((level) => (
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

          <div className="flex flex-wrap items-center gap-2">
            {/* Mode segmented control */}
            <div className="inline-flex items-center rounded-md border bg-background p-0.5 text-sm">
              <Button
                type="button"
                variant={mode.kind === 'pause' ? 'secondary' : 'ghost'}
                size="sm"
                className="h-7 gap-1.5 px-2"
                onClick={() => setMode({ kind: 'pause' })}
                aria-pressed={mode.kind === 'pause'}
              >
                <Pause className="h-3.5 w-3.5" />
                Pause
              </Button>
              <Button
                type="button"
                variant={mode.kind === 'live' ? 'secondary' : 'ghost'}
                size="sm"
                className="h-7 gap-1.5 px-2"
                onClick={() => setMode({ kind: 'live' })}
                aria-pressed={mode.kind === 'live'}
              >
                <Play className="h-3.5 w-3.5" />
                Live
              </Button>
              <div className="inline-flex items-center">
                <Button
                  type="button"
                  variant={mode.kind === 'interval' ? 'secondary' : 'ghost'}
                  size="sm"
                  className="h-7 gap-1.5 rounded-r-none px-2"
                  onClick={() => {
                    const ms =
                      mode.kind === 'interval' ? mode.ms : lastIntervalMs
                    setMode({ kind: 'interval', ms })
                  }}
                  aria-pressed={mode.kind === 'interval'}
                >
                  <Timer className="h-3.5 w-3.5" />
                  Every{' '}
                  {formatIntervalLabel(
                    mode.kind === 'interval' ? mode.ms : lastIntervalMs,
                  )}
                </Button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      type="button"
                      variant={mode.kind === 'interval' ? 'secondary' : 'ghost'}
                      size="sm"
                      className="h-7 rounded-l-none border-l px-1.5"
                      aria-label="Choose interval"
                    >
                      <ChevronDown className="h-3.5 w-3.5" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    {INTERVAL_OPTIONS_MS.map((ms) => (
                      <DropdownMenuItem
                        key={ms}
                        onSelect={() => {
                          setLastIntervalMs(ms)
                          setMode({ kind: 'interval', ms })
                        }}
                      >
                        Every {formatIntervalLabel(ms)}
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            </div>

            {/* Status: buffered count, countdown, manual flush */}
            <div className="flex items-center gap-2 text-xs text-muted-foreground">
              {mode.kind === 'pause' && (
                <span>Paused · stream closed</span>
              )}
              {mode.kind === 'live' && (
                <span className="flex items-center gap-2">
                  <span>
                    Live · {logs.length.toLocaleString()} lines
                    {lps > 0.5 && ` · ~${Math.round(lps).toLocaleString()} lps`}
                  </span>
                  {lps >= LPS_SAMPLED_THRESHOLD && (
                    <Badge
                      variant="outline"
                      className="h-5 gap-1 px-1.5 text-[10px] border-amber-500/40 text-amber-700 dark:text-amber-400 bg-amber-500/10"
                    >
                      sampled
                    </Badge>
                  )}
                </span>
              )}
              {mode.kind === 'interval' && (
                <>
                  <span>
                    {nextTickAt != null
                      ? `Next refresh in ${Math.max(
                          0,
                          Math.ceil((nextTickAt - now) / 1000),
                        )}s`
                      : 'Refreshing…'}
                    {bufferedCount > 0 &&
                      ` · +${bufferedCount.toLocaleString()} buffered`}
                    {droppedSinceFlush > 0 &&
                      ` · ${droppedSinceFlush.toLocaleString()} dropped`}
                  </span>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="h-6 gap-1 px-2 text-xs"
                    onClick={flushNow}
                  >
                    <RefreshCw className="h-3 w-3" />
                    Refresh now
                  </Button>
                </>
              )}
            </div>

            <div className="ml-auto">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setShowAdvanced(!showAdvanced)}
                className="text-muted-foreground hover:text-foreground"
              >
                Advanced Options
                {showAdvanced ? (
                  <ChevronUp className="ml-2 h-4 w-4" />
                ) : (
                  <ChevronDown className="ml-2 h-4 w-4" />
                )}
              </Button>
            </div>
          </div>

          {showAdvanced && (
            <div className="pt-4 border-t border-border space-y-3">
              <FilterBar
                onStartDateChange={setStartDate}
                onEndDateChange={setEndDate}
                onTailLinesChange={(lines) => setTail(lines)}
                startDate={startDate}
                endDate={endDate}
                tailLines={tail}
              />
              <div className="flex items-center gap-2">
                <Checkbox
                  id="show-timestamps"
                  checked={showTimestamps}
                  onCheckedChange={(checked) =>
                    setShowTimestamps(checked === true)
                  }
                />
                <Label
                  htmlFor="show-timestamps"
                  className="text-sm font-normal cursor-pointer"
                >
                  Show timestamps
                </Label>
              </div>
            </div>
          )}
        </div>
        {/* Logs Display — fills available viewport height. The 360px subtrahend
            accounts for the shell header (~64px), Runtime Live/History tabs
            (~40px), page padding, and the two toolbar rows above the log pane.
            Keep this in sync with ProjectRuntime so the page never produces a
            second outer scrollbar that would hide the tabs while scrolling
            logs. min-h floors the pane on short viewports so the empty state
            is still legible. */}
        <div className="border-t border-border">
          {!selectedTarget ? (
            <div className="h-[calc(100vh-360px)] min-h-[300px] flex items-center justify-center text-muted-foreground">
              <div className="text-center">
                <AlertCircle className="h-12 w-12 mx-auto mb-3 opacity-50" />
                <p className="text-sm">Select an environment to view logs</p>
              </div>
            </div>
          ) : (
            <div
              ref={parentRef}
              className={cn(
                'h-[calc(100vh-360px)] min-h-[300px] overflow-auto px-3 py-2 font-mono text-xs bg-background text-foreground select-text',
                mode.kind === 'live' &&
                  connectionStatus === 'connecting' &&
                  'opacity-50'
              )}
              onScroll={handleScroll}
              // Mark user intent on any explicit scroll input so handleScroll
              // can distinguish a real "user scrolled away" event from a
              // virtualizer-reflow scroll event. Without this gate, every
              // multi-line JSON log that grows post-measurement disengages
              // follow mode unprompted.
              onWheel={markUserIntent}
              onTouchMove={markUserIntent}
              onKeyDown={(e) => {
                // Arrow keys, PageUp/Down, Home/End, Space — anything that
                // moves the scrollbar from the keyboard.
                if (
                  e.key === 'ArrowUp' ||
                  e.key === 'ArrowDown' ||
                  e.key === 'PageUp' ||
                  e.key === 'PageDown' ||
                  e.key === 'Home' ||
                  e.key === 'End' ||
                  e.key === ' '
                ) {
                  markUserIntent()
                }
              }}
              tabIndex={0}
            >
              <div
                style={{
                  height: `${virtualizer.getTotalSize()}px`,
                  width: '100%',
                  position: 'relative',
                }}
              >
                {virtualizer.getVirtualItems().map((virtualRow) => {
                  const raw = filteredLogs[virtualRow.index]
                  if (raw === undefined) return null
                  // Fresh-batch animation runs only when no level filter is
                  // active. With a filter on, virtualRow.index points into
                  // the filtered rope and lastBatchStart points into the
                  // unfiltered one — the comparison is meaningless. Skipping
                  // the animation under filter is the cheaper fix than
                  // translating indices and matches the user's mental model
                  // ("I'm focused on these levels, don't dazzle me").
                  const isFresh =
                    selectedLevels.length === 0 &&
                    virtualRow.index >= lastBatchStart
                  return (
                    <div
                      key={virtualRow.key}
                      data-index={virtualRow.index}
                      ref={virtualizer.measureElement}
                      style={{
                        position: 'absolute',
                        top: `${virtualRow.start}px`,
                        left: 0,
                        width: '100%',
                        // No fixed height — the row sizes to its (possibly
                        // wrapped) content and measureElement reports it back.
                      }}
                      className={cn(isFresh && 'log-fresh-line')}
                    >
                      <LiveLogRow
                        raw={raw}
                        columns={columns}
                        searchTerm={searchTerm}
                        isHighlighted={
                          virtualRow.index === currentMatchIndex
                        }
                        serviceLabel={selectedContainerServiceName}
                      />
                    </div>
                  )
                })}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'

export type LiveLogLevel = 'ERROR' | 'WARN' | 'INFO' | 'DEBUG' | 'TRACE'

export interface LiveLogLine {
  level: LiveLogLevel
  // ISO timestamp if the server sent one (server adds it when ?timestamps=true).
  // We keep it on the row so the Timestamp column can render without re-parsing.
  timestamp?: string
  // Body without the leading timestamp (if we stripped one).
  message: string
}

export interface UseLogStreamOptions {
  wsUrl: string
  onError?: (error: string) => void
  maxLogs?: number
}

// Bounded tail buffer — older lines fall off the top when the buffer is
// full, matching how `docker logs --tail 1000 --follow` behaves. Keeps the
// browser responsive even on a chatty service that emits hundreds of lines
// per second over a long session. The history viewer is where users go when
// they need older data.
const DEFAULT_MAX_LOGS = 1000

export interface UseLogStreamReturn {
  logs: LiveLogLine[]
  filteredLogs: LiveLogLine[]
  connectionStatus: 'connecting' | 'connected' | 'error'
  errorMessage: string
  searchTerm: string
  selectedLevels: LiveLogLevel[]
  currentMatchIndex: number
  autoScroll: boolean
  showTimestamps: boolean
  paused: boolean
  bufferedCount: number
  parentRef: React.RefObject<HTMLDivElement | null>
  virtualizer: ReturnType<typeof useVirtualizer<HTMLDivElement, Element>>
  setSearchTerm: (term: string) => void
  setSelectedLevels: React.Dispatch<React.SetStateAction<LiveLogLevel[]>>
  setAutoScroll: (scroll: boolean) => void
  setShowTimestamps: (show: boolean) => void
  setPaused: (paused: boolean) => void
  resumeAndFlush: () => void
  scrollToBottom: () => void
  scrollToMatch: (index: number) => void
  handleScroll: (event: React.UIEvent<HTMLDivElement>) => void
  handleNextMatch: () => void
  handlePrevMatch: () => void
}

// Detect a leading ISO-8601 timestamp the server prepended when the client
// asked for ?timestamps=true. Docker emits `2025-05-30T10:40:00.123456789Z `
// (RFC 3339 with nano precision). We strip it off the body and keep it as a
// structured field so the Timestamp column can render without a second parse.
const TIMESTAMP_PREFIX = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)\s+/

// Severity inference. Container stdout has no native level — we look for the
// shape most loggers emit: a bracketed [LEVEL] token, an UPPERCASE level word
// near the start of the line, or known prefixes like "panic:" / "fatal:".
// Anything we can't classify falls through to INFO so the row still renders
// and the user can still filter the noise out.
const LEVEL_PATTERNS: Array<[LiveLogLevel, RegExp]> = [
  ['ERROR', /\b(ERROR|ERR|FATAL|PANIC|EMERG|CRIT)\b|\bpanic:|\bfatal:/i],
  ['WARN', /\b(WARN|WARNING)\b/i],
  ['DEBUG', /\b(DEBUG|DBG)\b/i],
  ['TRACE', /\b(TRACE|TRC)\b/i],
  ['INFO', /\b(INFO|NOTICE)\b/i],
]

function inferLevel(message: string): LiveLogLevel {
  // Only scan the first ~120 chars — the level word is almost always near
  // the start of the line; scanning the whole body would misclassify a stack
  // trace that happens to contain the word "warning" later on.
  const head = message.slice(0, 120)
  for (const [level, pattern] of LEVEL_PATTERNS) {
    if (pattern.test(head)) return level
  }
  return 'INFO'
}

function parseLine(raw: string): LiveLogLine {
  const tsMatch = raw.match(TIMESTAMP_PREFIX)
  const timestamp = tsMatch?.[1]
  const message = timestamp ? raw.slice(tsMatch![0].length) : raw
  return {
    level: inferLevel(message),
    timestamp,
    message,
  }
}

// Initial estimate for an unmeasured row. Actual height is measured per row
// (see `measureElement` below) because log lines are often long JSON payloads
// that wrap to several visual lines — a fixed height made wrapped content
// overflow its slot and render on top of the rows below.
const ROW_HEIGHT_PX = 22

export function useLogStream({
  wsUrl,
  onError,
  maxLogs = DEFAULT_MAX_LOGS,
}: UseLogStreamOptions): UseLogStreamReturn {
  const [logs, setLogs] = useState<LiveLogLine[]>([])
  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'connected' | 'error'
  >('connecting')
  const [errorMessage, setErrorMessage] = useState('')
  const [searchTerm, setSearchTerm] = useState('')
  const [selectedLevels, setSelectedLevels] = useState<LiveLogLevel[]>([])
  const [currentMatchIndex, setCurrentMatchIndex] = useState(-1)
  const [autoScroll, setAutoScroll] = useState(true)
  const [showTimestamps, setShowTimestamps] = useState(false)
  // Paused = freeze the visible rope and keep accumulating in a background
  // buffer. Implemented as both a ref (read inside the WS onmessage closure
  // without re-subscribing) and a state (so React re-renders the toolbar
  // when it toggles). See the freeze/resume effect below.
  const [paused, setPausedState] = useState(false)
  const pausedRef = useRef(false)
  const [bufferedCount, setBufferedCount] = useState(0)
  const parentRef = useRef<HTMLDivElement>(null)
  const wsRef = useRef<WebSocket | null>(null)
  const isConnectingRef = useRef(false)
  // Buffer incoming WS frames and flush once per animation frame. Without this,
  // a high-volume log source produces one setState per line, which makes React
  // re-render the virtualizer for every byte and freezes the tab.
  const pendingLogsRef = useRef<LiveLogLine[]>([])
  const flushHandleRef = useRef<number | null>(null)

  const filteredLogs = useMemo(() => {
    if (selectedLevels.length === 0 && !searchTerm) return logs
    const term = searchTerm.toLowerCase()
    return logs.filter((log) => {
      if (
        selectedLevels.length > 0 &&
        !selectedLevels.includes(log.level)
      ) {
        return false
      }
      if (term && !log.message.toLowerCase().includes(term)) {
        return false
      }
      return true
    })
  }, [logs, searchTerm, selectedLevels])

  const virtualizer = useVirtualizer({
    count: filteredLogs.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT_PX,
    measureElement: (el) => el.getBoundingClientRect().height,
    overscan: 20,
  })

  // WebSocket connection effect
  useEffect(() => {
    if (isConnectingRef.current) return

    setLogs([])
    setErrorMessage('')
    setBufferedCount(0)
    isConnectingRef.current = true
    pendingLogsRef.current = []

    const scheduleFlush = () => {
      if (flushHandleRef.current != null) return
      flushHandleRef.current = requestAnimationFrame(() => {
        flushHandleRef.current = null
        const incoming = pendingLogsRef.current
        if (incoming.length === 0) return
        // While paused, keep the buffer growing but don't commit to React
        // state — that's what makes the visible rope "freeze". We still cap
        // the buffer at maxLogs so a paused tab on a chatty service can't
        // grow without bound.
        if (pausedRef.current) {
          if (incoming.length > maxLogs) {
            pendingLogsRef.current = incoming.slice(-maxLogs)
          }
          setBufferedCount(pendingLogsRef.current.length)
          return
        }
        pendingLogsRef.current = []
        setLogs((prev) => {
          const next =
            prev.length + incoming.length <= maxLogs
              ? [...prev, ...incoming]
              : [...prev, ...incoming].slice(-maxLogs)
          return next
        })
      })
    }

    const enqueue = (line: string) => {
      pendingLogsRef.current.push(parseLine(line))
      scheduleFlush()
    }

    try {
      // Add timestamps query parameter to request server-side timestamps
      const url = new URL(
        wsUrl,
        typeof window !== 'undefined'
          ? window.location.origin
          : 'http://localhost'
      )
      url.searchParams.set('timestamps', showTimestamps.toString())

      const ws = new WebSocket(url.toString())

      ws.onopen = () => {
        setConnectionStatus('connected')
        setErrorMessage('')
      }

      ws.onmessage = (event) => {
        try {
          const parsed = JSON.parse(event.data)

          if (parsed.error && parsed.stack) {
            enqueue(`ERROR: ${parsed.error}\n${parsed.stack}`)
          } else if (parsed.message) {
            enqueue(parsed.message)
          } else if (parsed.log) {
            enqueue(parsed.log)
          } else {
            enqueue(JSON.stringify(parsed, null, 2))
          }
        } catch {
          const line = event.data.trim()
          if (line) {
            enqueue(line)
          }
        }
      }

      ws.onerror = () => {
        setConnectionStatus('error')
        const msg = 'Failed to connect to logs stream'
        setErrorMessage(msg)
        onError?.(msg)
      }

      ws.onclose = () => {
        setConnectionStatus('error')
        isConnectingRef.current = false
      }

      wsRef.current = ws
    } catch (error) {
      setConnectionStatus('error')
      const msg = error instanceof Error ? error.message : 'Connection failed'
      setErrorMessage(msg)
      onError?.(msg)
      isConnectingRef.current = false
    }

    return () => {
      if (wsRef.current) {
        wsRef.current.close()
      }
      if (flushHandleRef.current != null) {
        cancelAnimationFrame(flushHandleRef.current)
        flushHandleRef.current = null
      }
      pendingLogsRef.current = []
      isConnectingRef.current = false
    }
  }, [wsUrl, onError, showTimestamps, maxLogs])

  // Auto-scroll effect with proper timing for virtualizer. Disabled while
  // paused so the user's read position is preserved.
  useEffect(() => {
    if (paused) return
    if (autoScroll && parentRef.current && logs.length > 0) {
      requestAnimationFrame(() => {
        if (parentRef.current) {
          parentRef.current.scrollTop = parentRef.current.scrollHeight
        }
      })
    }
  }, [logs, autoScroll, paused])

  // Handle scroll: only flip autoScroll based on user intent when we're NOT
  // paused. While paused, scrolling around is reading-mode and should not
  // silently re-arm tail-follow.
  const handleScroll = (event: React.UIEvent<HTMLDivElement>) => {
    if (pausedRef.current) return
    const { scrollTop, scrollHeight, clientHeight } = event.currentTarget
    const isAtBottom = scrollHeight - scrollTop - clientHeight < 1
    setAutoScroll(isAtBottom)
  }

  const setPaused = useCallback((next: boolean) => {
    pausedRef.current = next
    setPausedState(next)
  }, [])

  const resumeAndFlush = useCallback(() => {
    pausedRef.current = false
    setPausedState(false)
    const incoming = pendingLogsRef.current
    pendingLogsRef.current = []
    setBufferedCount(0)
    if (incoming.length > 0) {
      setLogs((prev) => {
        const next =
          prev.length + incoming.length <= maxLogs
            ? [...prev, ...incoming]
            : [...prev, ...incoming].slice(-maxLogs)
        return next
      })
    }
    setAutoScroll(true)
    requestAnimationFrame(() => {
      if (parentRef.current) {
        parentRef.current.scrollTop = parentRef.current.scrollHeight
      }
    })
  }, [maxLogs])

  const scrollToBottom = useCallback(() => {
    setAutoScroll(true)
    requestAnimationFrame(() => {
      if (parentRef.current) {
        parentRef.current.scrollTop = parentRef.current.scrollHeight
      }
    })
  }, [])

  // Search and match scrolling
  const scrollToMatch = useCallback(
    (index: number) => {
      if (index >= 0 && index < filteredLogs.length) {
        setCurrentMatchIndex(index)
        virtualizer.scrollToIndex(index, { align: 'center' })
      }
    },
    [filteredLogs.length, virtualizer]
  )

  const handleNextMatch = () => {
    if (filteredLogs.length === 0) return
    const nextIndex = (currentMatchIndex + 1) % filteredLogs.length
    scrollToMatch(nextIndex)
  }

  const handlePrevMatch = () => {
    if (filteredLogs.length === 0) return
    const prevIndex =
      currentMatchIndex <= 0 ? filteredLogs.length - 1 : currentMatchIndex - 1
    scrollToMatch(prevIndex)
  }

  return {
    logs,
    filteredLogs,
    connectionStatus,
    errorMessage,
    searchTerm,
    selectedLevels,
    currentMatchIndex,
    autoScroll,
    showTimestamps,
    paused,
    bufferedCount,
    parentRef,
    virtualizer,
    setSearchTerm,
    setSelectedLevels,
    setAutoScroll,
    setShowTimestamps,
    setPaused,
    resumeAndFlush,
    scrollToBottom,
    scrollToMatch,
    handleScroll,
    handleNextMatch,
    handlePrevMatch,
  }
}

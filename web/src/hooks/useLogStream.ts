// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { connectLogStream } from './logStreamConnection'
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

export function buildLogStreamUrl(
  wsUrl: string,
  origin: string,
  showTimestamps: boolean,
  tail: number
): string {
  const url = new URL(wsUrl, origin)
  url.searchParams.set('timestamps', showTimestamps.toString())
  // Bound the backlog at Docker, not only after it reaches the browser. If
  // `tail` is omitted the API defaults to `all`, which can replay every log
  // line from every restart before the client-side ring buffer trims it.
  url.searchParams.set('tail', tail.toString())
  return url.toString()
}

// The buffer after `incoming` arrives, bounded to the last `maxLogs` lines.
// With `replace`, `incoming` starts a reconnected socket's replay of the same
// tail, so it takes the buffer's place instead of repeating lines already in
// it.
export function mergeLogLines<T>(
  prev: T[],
  incoming: T[],
  maxLogs: number,
  replace: boolean
): T[] {
  const next = replace ? incoming : [...prev, ...incoming]
  return next.length <= maxLogs ? next : next.slice(-maxLogs)
}

// One frame of the log stream: a log line, or the server saying the stream
// failed (`{"error": ..., "detail" | "stack": ...}`, sent when Docker or the
// worker cannot be reached). Both are shown as rows; only a log line can start
// a reconnected socket's replay of the tail.
export interface LogFrame {
  kind: 'line' | 'error'
  text: string
}

const ERROR_FRAME_KEYS = new Set(['error', 'detail', 'stack'])

function isErrorFrame(parsed: unknown): boolean {
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    return false
  }
  const frame = parsed as Record<string, unknown>
  return (
    typeof frame.error === 'string' &&
    Object.keys(frame).every((key) => ERROR_FRAME_KEYS.has(key))
  )
}

// The row a frame shows, and whether it is a log line or an error. `null`
// for a blank frame.
export function parseLogFrame(data: string): LogFrame | null {
  try {
    const parsed = JSON.parse(data)
    const kind = isErrorFrame(parsed) ? 'error' : 'line'
    if (parsed.error && parsed.stack) {
      return { kind, text: `ERROR: ${parsed.error}\n${parsed.stack}` }
    }
    if (parsed.message) return { kind, text: parsed.message }
    if (parsed.log) return { kind, text: parsed.log }
    return { kind, text: JSON.stringify(parsed, null, 2) }
  } catch {
    const line = data.trim()
    return line ? { kind: 'line', text: line } : null
  }
}

// Decides which frame replaces the buffer after a reconnect. A replacement
// socket replays the tail the buffer already holds, so its first log line
// replaces the buffer. An error sent before that line (the reconnect failed)
// is appended and leaves the lines on screen alone; the replace stays armed
// for the replay that follows, on this socket or the next.
export function createReplayGate() {
  let armed = false
  return {
    socketReplaced() {
      armed = true
    },
    replacesBuffer(frame: LogFrame): boolean {
      if (!armed || frame.kind !== 'line') return false
      armed = false
      return true
    },
    reset() {
      armed = false
    },
  }
}

export interface UseLogStreamReturn {
  logs: LiveLogLine[]
  filteredLogs: LiveLogLine[]
  connectionStatus: 'connecting' | 'connected' | 'complete' | 'error'
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
  retry: () => void
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
  'use no memo'
  const [logs, setLogs] = useState<LiveLogLine[]>([])
  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'connected' | 'complete' | 'error'
  >('connecting')
  const [errorMessage, setErrorMessage] = useState('')
  const [searchTerm, setSearchTerm] = useState('')
  const [selectedLevels, setSelectedLevels] = useState<LiveLogLevel[]>([])
  const [currentMatchIndex, setCurrentMatchIndex] = useState(-1)
  const [autoScroll, setAutoScroll] = useState(true)
  const [showTimestamps, setShowTimestamps] = useState(true)
  const [retryGeneration, setRetryGeneration] = useState(0)
  const retry = useCallback(() => setRetryGeneration((n) => n + 1), [])
  // Paused = freeze the visible rope and keep accumulating in a background
  // buffer. Implemented as both a ref (read inside the WS onmessage closure
  // without re-subscribing) and a state (so React re-renders the toolbar
  // when it toggles). See the freeze/resume effect below.
  const [paused, setPausedState] = useState(false)
  const pausedRef = useRef(false)
  const [bufferedCount, setBufferedCount] = useState(0)
  const parentRef = useRef<HTMLDivElement>(null)
  // Buffer incoming WS frames and flush once per animation frame. Without this,
  // a high-volume log source produces one setState per line, which makes React
  // re-render the virtualizer for every byte and freezes the tab.
  const pendingLogsRef = useRef<LiveLogLine[]>([])
  const flushHandleRef = useRef<number | null>(null)
  // Set when a reconnected socket starts replaying the tail: the next commit
  // replaces the visible lines instead of appending to them.
  const replaceOnFlushRef = useRef(false)
  const replayGateRef = useRef(createReplayGate())

  const filteredLogs = useMemo(() => {
    if (selectedLevels.length === 0 && !searchTerm) return logs
    const term = searchTerm.toLowerCase()
    return logs.filter((log) => {
      if (selectedLevels.length > 0 && !selectedLevels.includes(log.level)) {
        return false
      }
      if (term && !log.message.toLowerCase().includes(term)) {
        return false
      }
      return true
    })
  }, [logs, searchTerm, selectedLevels])

  // TanStack Virtual deliberately returns mutable measurement functions; the
  // React Compiler correctly leaves this hook un-memoized.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: filteredLogs.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT_PX,
    measureElement: (el) => el.getBoundingClientRect().height,
    overscan: 20,
  })

  // WebSocket connection effect
  useEffect(() => {
    setLogs([])
    setErrorMessage('')
    setBufferedCount(0)
    setConnectionStatus('connecting')
    pendingLogsRef.current = []
    replaceOnFlushRef.current = false
    replayGateRef.current.reset()

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
        const replace = replaceOnFlushRef.current
        replaceOnFlushRef.current = false
        setLogs((prev) => mergeLogLines(prev, incoming, maxLogs, replace))
      })
    }

    const enqueue = (line: string) => {
      pendingLogsRef.current.push(parseLine(line))
      scheduleFlush()
    }

    const disconnect = connectLogStream(
      () =>
        new WebSocket(
          buildLogStreamUrl(
            wsUrl,
            window.location.origin,
            showTimestamps,
            maxLogs
          )
        ),
      (status, message = '') => {
        setConnectionStatus(status)
        setErrorMessage(message)
        if (message) onError?.(message)
      },
      (event, socketReplaced) => {
        const gate = replayGateRef.current
        if (socketReplaced) gate.socketReplaced()
        const frame = parseLogFrame(event.data)
        if (!frame) return
        if (gate.replacesBuffer(frame)) {
          // Lines queued from the dropped socket are in the replay too.
          pendingLogsRef.current = []
          replaceOnFlushRef.current = true
          if (pausedRef.current) setBufferedCount(0)
        }
        enqueue(frame.text)
      }
    )

    return () => {
      disconnect()
      if (flushHandleRef.current != null) {
        cancelAnimationFrame(flushHandleRef.current)
        flushHandleRef.current = null
      }
      pendingLogsRef.current = []
      replaceOnFlushRef.current = false
      replayGateRef.current.reset()
    }
  }, [wsUrl, onError, showTimestamps, maxLogs, retryGeneration])

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
    const replace = replaceOnFlushRef.current
    replaceOnFlushRef.current = false
    if (incoming.length > 0 || replace) {
      setLogs((prev) => mergeLogLines(prev, incoming, maxLogs, replace))
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
    retry,
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

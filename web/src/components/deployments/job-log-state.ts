// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure state logic for the deployment build-log viewer.
 *
 * The viewer has two transports for the same JSONL log file:
 *
 * - `GET .../jobs/{job_id}/logs?tail=N` returns a bounded suffix. It answers 200 with an
 *   empty body for a job that has not written anything yet and 404 for a
 *   finished job whose log is gone.
 * - `GET .../jobs/{job_id}/logs/tail` is a WebSocket that replays the recent
 *   lines and then follows the file.
 *
 * A finished job only ever needs the HTTP read. A running job prefers the
 * socket and falls back to polling the HTTP read whenever the socket cannot
 * connect or drops, so the panel never waits on a socket that will not open.
 * Everything here is side-effect free so the transitions can be unit tested.
 */

export interface JobLogEntry {
  level: string
  message: string
  timestamp: string
  line: number
}

/**
 * Lifecycle phase of a job, as far as its log is concerned.
 *
 * - `waiting`: queued or blocked on a dependency; nothing written yet.
 * - `live`: running; the log is still growing.
 * - `finished`: terminal; the log is complete (or was never written).
 */
export type JobLogPhase = 'waiting' | 'live' | 'finished'

/** Small live polls bound object-store requests; terminal reads use the viewer cap. */
export function snapshotTailLines(phase: JobLogPhase): number {
  return phase === 'finished' ? MAX_VIEWER_LINES : 128
}

/** Connection state of the live tail socket. Only meaningful in `live`. */
export type SocketState = 'idle' | 'connecting' | 'open' | 'failed'

/** Result of an HTTP log read. Errors other than 404 are thrown. */
export type JobLogSnapshot =
  { kind: 'content'; entries: JobLogEntry[] } | { kind: 'gone'; detail: string }

/** How long a socket may stay in CONNECTING before we fall back to HTTP. */
export const SOCKET_CONNECT_TIMEOUT_MS = 5_000
/** HTTP poll cadence while the live socket is unavailable. */
export const FALLBACK_POLL_INTERVAL_MS = 2_000
/** HTTP poll cadence for a job that has not started yet. */
export const WAITING_POLL_INTERVAL_MS = 2_500
const MAX_RECONNECT_DELAY_MS = 15_000
/**
 * Most recent lines the viewer keeps in memory. HTTP reads request a bounded
 * suffix and the socket streams indefinitely, so the browser-side buffer
 * is bounded here rather than growing with the build. Older lines fall off
 * the top, and the viewer says so.
 */
export const MAX_VIEWER_LINES = 10_000

export function jobLogPhase(status: string): JobLogPhase {
  switch (status) {
    case 'running':
      return 'live'
    case 'pending':
    case 'waiting':
      return 'waiting'
    default:
      // success, failure, cancelled, skipped -- and any status this console
      // does not know yet. Treating an unknown status as finished means a
      // single HTTP read, which can never hang.
      return 'finished'
  }
}

/** Backoff before reconnect attempt `attempt` (0-based): 1s, 2s, 4s, 8s, 15s… */
export function reconnectDelayMs(attempt: number): number {
  return Math.min(1_000 * 2 ** Math.max(0, attempt), MAX_RECONNECT_DELAY_MS)
}

/** Whether the HTTP log read should run at all for this phase/socket state. */
export function shouldReadSnapshot(
  phase: JobLogPhase,
  socketState: SocketState
): boolean {
  return phase !== 'live' || socketState === 'failed'
}

/** HTTP poll interval for this phase/socket state, or `false` for no polling. */
export function snapshotPollInterval(
  phase: JobLogPhase,
  socketState: SocketState,
  snapshot: JobLogSnapshot | undefined
): number | false {
  if (snapshot?.kind === 'gone') return false
  if (phase === 'waiting') return WAITING_POLL_INTERVAL_MS
  if (phase === 'live' && socketState === 'failed') {
    return FALLBACK_POLL_INTERVAL_MS
  }
  return false
}

const EDGE_NEWLINES = /^[\r\n]+|[\r\n]+$/g

function cleanMessage(message: string): string {
  return message.replace(EDGE_NEWLINES, '')
}

function isLogEntryShape(value: unknown): value is {
  level: string
  message: string
  timestamp?: string
  line: number
} {
  if (typeof value !== 'object' || value === null) return false
  const candidate = value as Record<string, unknown>
  return (
    typeof candidate.level === 'string' &&
    typeof candidate.message === 'string' &&
    typeof candidate.line === 'number'
  )
}

/**
 * Parse one raw log line (JSONL entry or plain text) into an entry.
 * `fallbackLine` numbers lines that do not carry their own line number.
 */
export function parseLogLine(
  raw: string,
  fallbackLine: number,
  now: () => string = () => new Date().toISOString()
): JobLogEntry {
  try {
    const parsed: unknown = JSON.parse(raw)
    if (isLogEntryShape(parsed)) {
      return {
        level: parsed.level,
        message: cleanMessage(parsed.message),
        timestamp: parsed.timestamp ?? now(),
        line: parsed.line,
      }
    }
    if (
      typeof parsed === 'object' &&
      parsed !== null &&
      typeof (parsed as { message?: unknown }).message === 'string'
    ) {
      return {
        level: 'info',
        message: cleanMessage((parsed as { message: string }).message),
        timestamp: now(),
        line: fallbackLine,
      }
    }
  } catch {
    // Plain-text line; handled below.
  }
  return {
    level: 'info',
    message: cleanMessage(raw),
    timestamp: now(),
    line: fallbackLine,
  }
}

/** Parse the body of the HTTP log read (JSONL, one entry per line). */
export function parseJobLogContent(
  content: string,
  now?: () => string,
  maxLines: number = MAX_VIEWER_LINES
): JobLogEntry[] {
  const rawLines = content.split('\n').filter((raw) => raw.trim() !== '')
  // Only the retained tail is parsed, so a long log costs one split rather
  // than one JSON.parse per line on every poll.
  const start = Math.max(0, rawLines.length - maxLines)
  const entries: JobLogEntry[] = []
  let lastLine = start
  for (let index = start; index < rawLines.length; index += 1) {
    const entry = parseLogLine(rawLines[index], lastLine + 1, now)
    lastLine = Math.max(lastLine, entry.line)
    entries.push(entry)
  }
  return entries
}

export type StreamMessage =
  { kind: 'entry'; entry: JobLogEntry } | { kind: 'error'; message: string }

/**
 * Classify one WebSocket message. The tail handler sends a JSON
 * `{ error, detail }` object (then closes) when it cannot open the log, and an
 * `ERROR: …` text frame when reading fails mid-stream; both mean the stream is
 * unusable and the viewer should fall back to HTTP.
 */
export function parseStreamMessage(
  data: string,
  lastLine: number,
  now?: () => string
): StreamMessage {
  try {
    const parsed: unknown = JSON.parse(data)
    if (
      typeof parsed === 'object' &&
      parsed !== null &&
      !isLogEntryShape(parsed) &&
      typeof (parsed as { error?: unknown }).error === 'string'
    ) {
      const { error, detail } = parsed as { error: string; detail?: unknown }
      return {
        kind: 'error',
        message: typeof detail === 'string' ? `${error}: ${detail}` : error,
      }
    }
  } catch {
    if (data.startsWith('ERROR: ')) {
      return { kind: 'error', message: data.slice('ERROR: '.length) }
    }
  }
  return { kind: 'entry', entry: parseLogLine(data, lastLine + 1, now) }
}

/**
 * Merge two line-numbered sequences, deduplicating by line number.
 *
 * The socket replays recent lines on every (re)connect and the HTTP read
 * returns the whole file, so the same line routinely arrives more than once.
 * The common case -- strictly newer lines appended to the end -- is a plain
 * concatenation; anything else falls back to a keyed merge.
 */
export function mergeLogEntries(
  existing: JobLogEntry[],
  incoming: JobLogEntry[],
  maxLines: number = MAX_VIEWER_LINES
): JobLogEntry[] {
  if (incoming.length === 0) return existing
  if (existing.length === 0) return keepLast(incoming, maxLines)
  const lastLine = existing[existing.length - 1].line
  let appendable = true
  let previous = lastLine
  for (const entry of incoming) {
    if (entry.line <= previous) {
      appendable = false
      break
    }
    previous = entry.line
  }
  if (appendable) return keepLast(existing.concat(incoming), maxLines)

  const byLine = new Map<number, JobLogEntry>()
  for (const entry of existing) byLine.set(entry.line, entry)
  let changed = false
  for (const entry of incoming) {
    if (!byLine.has(entry.line)) {
      byLine.set(entry.line, entry)
      changed = true
    }
  }
  if (!changed) return existing
  return keepLast(
    Array.from(byLine.values()).sort((a, b) => a.line - b.line),
    maxLines
  )
}

/** Missing absolute line numbers between retained entries, excluding the prefix. */
export function missingLogLines(entries: JobLogEntry[]): number {
  let missing = 0
  for (let index = 1; index < entries.length; index += 1) {
    missing += Math.max(0, entries[index].line - entries[index - 1].line - 1)
  }
  return missing
}

function keepLast(entries: JobLogEntry[], maxLines: number): JobLogEntry[] {
  return entries.length > maxLines
    ? entries.slice(entries.length - maxLines)
    : entries
}

/** What the log pane shows in place of (or alongside) log lines. */
export type JobLogBody =
  | 'lines'
  | 'loading'
  | 'connecting'
  | 'not-started'
  | 'waiting-for-output'
  | 'polling-for-output'
  | 'no-output'
  | 'gone'
  | 'error'

/** Transport notice shown above the pane, if any. */
export type JobLogNotice =
  'none' | 'connecting' | 'polling' | 'refresh-failed' | 'partial-log'

export interface JobLogViewInput {
  phase: JobLogPhase
  socketState: SocketState
  entryCount: number
  snapshot: {
    data: JobLogSnapshot | undefined
    isPending: boolean
    isError: boolean
  }
}

export interface JobLogView {
  body: JobLogBody
  notice: JobLogNotice
}

/**
 * Decide what the viewer renders. Every branch terminates in a concrete
 * state: the only transient ones (`loading`, `connecting`) are bounded by the
 * HTTP request and the socket connect timeout respectively.
 */
export function deriveJobLogView(input: JobLogViewInput): JobLogView {
  const { phase, socketState, entryCount, snapshot } = input
  const usingSocket = phase === 'live' && socketState !== 'failed'

  const notice = deriveNotice(input)

  if (entryCount > 0) return { body: 'lines', notice }

  if (snapshot.data?.kind === 'gone') return { body: 'gone', notice: 'none' }

  if (usingSocket) {
    return {
      body: socketState === 'open' ? 'waiting-for-output' : 'connecting',
      notice,
    }
  }

  // From here on the HTTP read is the source of truth.
  if (snapshot.isError && snapshot.data === undefined) {
    return { body: 'error', notice: 'none' }
  }
  if (snapshot.data === undefined) {
    // Waiting/polling phases already have a meaningful empty message, so a
    // first poll in flight need not blank the pane with a skeleton.
    if (phase === 'waiting') return { body: 'not-started', notice }
    if (phase === 'live') return { body: 'polling-for-output', notice }
    return { body: 'loading', notice: 'none' }
  }

  switch (phase) {
    case 'waiting':
      return { body: 'not-started', notice }
    case 'live':
      return { body: 'polling-for-output', notice }
    case 'finished':
      return { body: 'no-output', notice: 'none' }
  }
}

function deriveNotice({
  phase,
  socketState,
  entryCount,
  snapshot,
}: JobLogViewInput): JobLogNotice {
  // The complete log is gone (404), but lines streamed while the job ran are
  // still on screen. Never let that partial tail pass for the full log.
  if (snapshot.data?.kind === 'gone' && entryCount > 0) return 'partial-log'
  if (phase === 'live' && socketState === 'failed') {
    return snapshot.isError ? 'refresh-failed' : 'polling'
  }
  if (phase === 'live' && socketState === 'connecting' && entryCount > 0) {
    return 'connecting'
  }
  if (snapshot.isError && entryCount > 0 && phase !== 'live') {
    return 'refresh-failed'
  }
  return 'none'
}

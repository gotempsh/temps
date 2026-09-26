// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GlobalLogLine, LogLevel } from '@/api/client/types.gen'

export const LOG_LEVELS: LogLevel[] = [
  'ERROR',
  'WARN',
  'INFO',
  'DEBUG',
  'TRACE',
]

/**
 * Stable identity of a log line.
 *
 * The indexed store orders — and paginates — by `(timestamp, container_id,
 * line_id)`, so that triple is the only thing that identifies a line. `line_id`
 * is a decimal *string* (a 64-bit value seeded from Unix nanoseconds, well past
 * `Number.MAX_SAFE_INTEGER`): never parse it, only compare it.
 */
export const logLineKey = (line: {
  timestamp: string
  container_id?: string
  line_id: string
}) => `${line.timestamp}|${line.container_id ?? ''}|${line.line_id}`

/**
 * A retry or stale cursor can replay the inclusive keyset boundary. Keep the
 * first occurrence so the explorer never renders or counts one stored line
 * twice when pages overlap.
 */
export function uniqueLogLines(lines: GlobalLogLine[]): GlobalLogLine[] {
  const seen = new Set<string>()
  return lines.filter((line) => {
    const key = logLineKey(line)
    if (seen.has(key)) return false
    seen.add(key)
    return true
  })
}

export function logVolume(lines: GlobalLogLine[]) {
  const timestamps = lines
    .map((line) => Date.parse(line.timestamp))
    .filter(Number.isFinite)
  if (!timestamps.length) return { buckets: [], step: 60000 }
  const min = Math.min(...timestamps),
    max = Math.max(...timestamps)
  const step = Math.max(60000, Math.ceil((max - min + 1) / 36 / 60000) * 60000)
  const start = Math.floor(min / step) * step
  const buckets = Array.from(
    { length: Math.floor((max - start) / step) + 1 },
    (_, index) => ({
      time: start + index * step,
      ERROR: 0,
      WARN: 0,
      INFO: 0,
      DEBUG: 0,
      TRACE: 0,
    })
  )
  for (const line of lines) {
    const time = Date.parse(line.timestamp)
    if (!Number.isFinite(time)) continue
    const bucket = buckets[Math.floor((time - start) / step)]
    if (bucket && LOG_LEVELS.includes(line.level)) bucket[line.level] += 1
  }
  return { buckets, step }
}

/** Exact repeated messages are grouped; no guessed templates or unseen records. */
export function groupLogLines(
  lines: GlobalLogLine[],
  by: 'message' | 'service'
) {
  const groups = new Map<
    string,
    { label: string; count: number; errors: number; example: GlobalLogLine }
  >()
  for (const line of lines) {
    const key =
      by === 'message'
        ? line.message
        : JSON.stringify([
            line.project_id,
            line.external_service_id,
            line.service,
          ])
    const group = groups.get(key) ?? {
      label:
        by === 'message' ? line.message : `${line.owner} / ${line.service}`,
      count: 0,
      errors: 0,
      example: line,
    }
    group.count += 1
    if (line.level === 'ERROR') group.errors += 1
    groups.set(key, group)
  }
  return [...groups]
    .map(([id, group]) => ({ id, ...group }))
    .sort((a, b) => b.count - a.count || a.id.localeCompare(b.id))
}

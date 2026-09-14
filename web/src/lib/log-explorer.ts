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

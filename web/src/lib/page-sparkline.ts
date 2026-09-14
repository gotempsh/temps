// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { PagePathSparklinePoint } from '@/api/client/types.gen'

export function pageSparkline(
  points: PagePathSparklinePoint[],
  start: Date,
  end: Date
) {
  const days = Math.trunc((end.getTime() - start.getTime()) / 86400000)
  if (!Number.isFinite(days) || end < start) return []
  const unit =
    days <= 2 ? 'hour' : days <= 31 ? 'day' : days <= 180 ? 'week' : 'month'
  const floor = (time: number) => {
    const d = new Date(time)
    d.setUTCMinutes(0, 0, 0)
    if (unit !== 'hour') d.setUTCHours(0)
    if (unit === 'week')
      d.setUTCDate(d.getUTCDate() - ((d.getUTCDay() + 6) % 7))
    if (unit === 'month') d.setUTCDate(1)
    return d.getTime()
  }
  const counts = new Map<number, number>()
  for (const point of points) {
    const iso = point.timestamp.replace(' ', 'T')
    const time = Date.parse(/(?:Z|[+-]\d\d:\d\d)$/i.test(iso) ? iso : `${iso}Z`)
    if (Number.isFinite(time) && Number.isFinite(point.session_count))
      counts.set(floor(time), point.session_count)
  }
  const result: { time: number; sessions: number }[] = []
  const cursor = new Date(floor(start.getTime()))
  const last = floor(end.getTime())
  while (cursor.getTime() <= last && result.length < 1000) {
    const time = cursor.getTime()
    result.push({ time, sessions: counts.get(time) ?? 0 })
    if (unit === 'month') cursor.setUTCMonth(cursor.getUTCMonth() + 1)
    else
      cursor.setTime(
        time +
          (unit === 'hour' ? 3600000 : unit === 'day' ? 86400000 : 7 * 86400000)
      )
  }
  return result
}

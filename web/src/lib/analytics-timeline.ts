// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
export interface AnalyticsTimelineRow {
  date: string
  count: number
}
const HOUR = 3_600_000
export function analyticsTimeline(
  rows: AnalyticsTimelineRow[],
  start: Date,
  end: Date
) {
  const first = Math.floor(start.getTime() / HOUR) * HOUR
  const last = Math.floor(end.getTime() / HOUR) * HOUR
  if (
    !Number.isFinite(first) ||
    !Number.isFinite(last) ||
    last < first ||
    last - first > 90 * 24 * HOUR
  )
    return []
  const counts = new Map<number, number>()
  for (const row of rows) {
    // The project API's date strings are UTC even when they omit a timezone.
    const iso = row.date.replace(' ', 'T')
    const time = Date.parse(/(?:Z|[+-]\d\d:\d\d)$/i.test(iso) ? iso : `${iso}Z`)
    if (!Number.isFinite(time) || !Number.isFinite(row.count)) continue
    const bucket = Math.floor(time / HOUR) * HOUR
    counts.set(bucket, (counts.get(bucket) ?? 0) + row.count)
  }
  return Array.from({ length: (last - first) / HOUR + 1 }, (_, index) => {
    const timestamp = first + index * HOUR
    return { timestamp, count: counts.get(timestamp) ?? 0 }
  })
}
export function formatAnalyticsTick(
  timestamp: number,
  start?: Date,
  end?: Date
) {
  const multiDay = start && end && end.getTime() - start.getTime() > 24 * HOUR
  return new Date(timestamp).toLocaleString(undefined, {
    ...(multiDay ? ({ month: 'short', day: 'numeric' } as const) : {}),
    hour: 'numeric',
  })
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CronExecutionFixture } from '../fixtures'

/** One selection feeds the overview, trend and paginated table. */
export function executionOverview(
  records: CronExecutionFixture[],
  from: string,
  to: string,
  query: string,
  result: string,
) {
  const start = Date.parse(from)
  const end = Date.parse(to)
  const search = query.trim().toLowerCase()
  const rows = records
    .filter((run) => {
      const time = Date.parse(run.executedAt)
      const succeeded = run.statusCode >= 200 && run.statusCode < 300
      return (
        time >= start &&
        time <= end &&
        run.path.toLowerCase().includes(search) &&
        (result === 'failed'
          ? !succeeded
          : result === 'succeeded'
            ? succeeded
            : true)
      )
    })
    .sort((a, b) => Date.parse(b.executedAt) - Date.parse(a.executedAt))
  const failed = rows.filter(
    (run) => run.statusCode < 200 || run.statusCode >= 300,
  ).length
  const durations = rows.map((run) => run.durationMs).sort((a, b) => a - b)
  const binCount = 24
  const interval = (end - start) / binCount
  const trend = Array.from({ length: binCount }, (_, index) => ({
    time: new Date(start + index * interval).toISOString(),
    succeeded: 0,
    failed: 0,
  }))
  for (const run of rows) {
    const index = Math.min(
      binCount - 1,
      Math.floor((Date.parse(run.executedAt) - start) / interval),
    )
    if (run.statusCode >= 200 && run.statusCode < 300) trend[index].succeeded++
    else trend[index].failed++
  }
  return {
    rows,
    failed,
    trend,
    successRate: rows.length ? (rows.length - failed) / rows.length : null,
    p95: durations.length
      ? durations[Math.ceil(durations.length * 0.95) - 1]
      : null,
  }
}

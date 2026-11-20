// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** One hour of an operation, as the trend endpoint returns it. */
export interface TrendPointLike {
  bucket_start: string
  count: number
  error_count: number
  p50_ms: number
  p95_ms: number
}

/** A deployment that went live, as the trend endpoint returns it. */
export interface DeploymentMarkerLike {
  deployment_id: number
  slug: string
  live_at: string
}

export interface TrendChartRow {
  /** Categorical x value, unique per hour. */
  hour: string
  ts: string
  p50: number
  p95: number
  count: number
}

const hourLabel = (date: Date) =>
  date.toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
  })

/** Chart rows, oldest first, one per hour with calls. */
export function trendChartRows(points: TrendPointLike[]): TrendChartRow[] {
  return [...points]
    .sort((a, b) => a.bucket_start.localeCompare(b.bucket_start))
    .map((point) => ({
      hour: `${hourLabel(new Date(point.bucket_start))} (${point.bucket_start})`,
      ts: point.bucket_start,
      p50: point.p50_ms,
      p95: point.p95_ms,
      count: point.count,
    }))
}

/** Strip the disambiguating timestamp from a row's `hour` for display. */
export function hourTick(value: string | number): string {
  return String(value).replace(/ \(.*\)$/, '')
}

/**
 * Snap each deployment to the chart row nearest the hour it went live in,
 * collapsing deployments that land on the same row into one marker.
 */
export function deploymentMarkers(
  rows: TrendChartRow[],
  deployments: DeploymentMarkerLike[]
): { x: string; label: string; title: string }[] {
  if (rows.length === 0) return []
  const rowMs = rows.map((row) => new Date(row.ts).getTime())
  const byRow = new Map<number, DeploymentMarkerLike[]>()
  for (const deployment of deployments) {
    const at = new Date(deployment.live_at).getTime()
    let best = 0
    for (let i = 1; i < rowMs.length; i++) {
      if (Math.abs(rowMs[i] - at) < Math.abs(rowMs[best] - at)) best = i
    }
    byRow.set(best, [...(byRow.get(best) ?? []), deployment])
  }
  return [...byRow.entries()]
    .sort(([a], [b]) => a - b)
    .map(([index, group]) => ({
      x: rows[index].hour,
      label:
        group.length > 1
          ? `#${group[0].deployment_id} +${group.length - 1}`
          : `#${group[0].deployment_id}`,
      title: group
        .map((d) => `Deployment #${d.deployment_id} (${d.slug}) went live`)
        .join('\n'),
    }))
}

/** "+40%" or "−30%" for a change ratio. */
export function changePercent(ratio: number | null | undefined): string {
  if (ratio == null || !Number.isFinite(ratio)) return '—'
  const pct = Math.round(ratio * 100)
  return pct > 0 ? `+${pct}%` : pct < 0 ? `−${Math.abs(pct)}%` : '0%'
}

/** Deployments a regression check can compare, newest first. */
export function comparableDeployments<
  T extends { status: string; finished_at?: number | null; created_at: number },
>(deployments: T[]): T[] {
  return deployments
    .filter(
      (d) =>
        ['deployed', 'completed', 'running', 'healthy'].includes(d.status) ||
        d.finished_at != null
    )
    .sort((a, b) => b.created_at - a.created_at)
}

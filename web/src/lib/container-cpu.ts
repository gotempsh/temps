// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Container CPU as recorded by the health monitor for a managed service, and
// how the console shows it. One definition everywhere: cores used, against
// the container's CPU limit when it has one — what the resources card shows.
//
// `container.cpu_percent` is stored in Docker's convention (100 == one core,
// so it exceeds 100 on a multi-core container). Printed as a bare "68.1%" it
// reads as a share of something; it is 0.68 cores.

import { formatCpuUsage } from '@/lib/cpu-format'

/** Cores used × 100, averaged since the previous health check. */
export const CONTAINER_CPU_METRIC = 'container.cpu_percent'
/** Share of the cores the container may use: its limit, else every host core. */
export const CONTAINER_CPU_UTILIZATION_METRIC =
  'container.cpu_utilization_percent'
/** The container's CPU limit in cores; 0 when uncapped. Recorded on every
 *  scrape where the limit is known, so removing a cap shows up as 0 rather
 *  than the last capped value lingering in the latest-values window. */
export const CONTAINER_CPU_LIMIT_METRIC = 'container.cpu_limit_cores'

export const CONTAINER_CPU_TILE_LABEL = 'CPU (cores, last scrape)'

export const CONTAINER_CPU_TILE_HINT =
  'Cores used, averaged since the previous health check (about every 30 seconds), ' +
  "against the container's CPU limit (for a cluster, the total across its " +
  'members). The resources card samples one second ' +
  'every few seconds, so its reading moves more.'

/** Chart and alert-picker labels for the container CPU metrics. */
export const CONTAINER_CPU_METRIC_LABELS: Record<string, string> = {
  [CONTAINER_CPU_METRIC]: 'CPU (cores)',
  [CONTAINER_CPU_UTILIZATION_METRIC]: 'CPU (% of limit)',
  [CONTAINER_CPU_LIMIT_METRIC]: 'CPU limit',
}

/**
 * The metrics an alert rule may be created on, from a page's metric list.
 * `container.cpu_percent` is dropped: a threshold on it is cores × 100, so a
 * user reading "0.68 cores" who types 2 would get a rule at 0.02 cores. CPU
 * alerts use the share of the limit instead, offered first.
 */
export function alertableMetrics(metrics: string[]): string[] {
  const rest = metrics.filter(
    (m) =>
      m !== CONTAINER_CPU_METRIC &&
      m !== CONTAINER_CPU_UTILIZATION_METRIC &&
      m !== CONTAINER_CPU_LIMIT_METRIC
  )
  return [CONTAINER_CPU_UTILIZATION_METRIC, ...rest]
}

/**
 * Y-axis width (px) for a chart of `name`, when it is a container CPU metric
 * read in cores. Its ticks ("0.050 cores", "12.40 cores") are about twice as
 * long as the "68.1%" a `_percent` width was sized for, and clip at 10px
 * text in a 52px axis. `undefined` for any other metric.
 */
export function containerCpuAxisWidth(name: string): number | undefined {
  return name === CONTAINER_CPU_METRIC ? 64 : undefined
}

/** The CPU tile's text, e.g. "0.68 / 6 cores", from the latest values. */
export function containerCpuTileText(
  lookup: (metric: string) => number | undefined
): string {
  return formatCpuUsage(
    lookup(CONTAINER_CPU_METRIC),
    lookup(CONTAINER_CPU_LIMIT_METRIC)
  )
}

/**
 * A container CPU value in its unit, or `null` for any other metric (or the
 * utilization, which is a plain percentage).
 */
export function formatContainerCpuMetric(
  name: string,
  value: number
): string | null {
  if (name === CONTAINER_CPU_METRIC) return formatCpuUsage(value, null)
  if (name === CONTAINER_CPU_LIMIT_METRIC) {
    if (value <= 0) return 'unlimited'
    const cores = parseFloat(value.toFixed(3))
    return `${cores} ${cores === 1 ? 'core' : 'cores'}`
  }
  return null
}

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
/** The container's CPU limit in cores; absent when uncapped. */
export const CONTAINER_CPU_LIMIT_METRIC = 'container.cpu_limit_cores'

export const CONTAINER_CPU_TILE_LABEL = 'CPU (cores, last scrape)'

export const CONTAINER_CPU_TILE_HINT =
  'Cores used, averaged since the previous health check (about every 30 seconds), ' +
  "against the container's CPU limit. The resources card samples one second " +
  'every few seconds, so its reading moves more.'

/** Chart and alert-picker labels for the container CPU metrics. */
export const CONTAINER_CPU_METRIC_LABELS: Record<string, string> = {
  [CONTAINER_CPU_METRIC]: 'CPU (cores)',
  [CONTAINER_CPU_UTILIZATION_METRIC]: 'CPU (% of limit)',
  [CONTAINER_CPU_LIMIT_METRIC]: 'CPU limit',
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
    const cores = parseFloat(value.toFixed(3))
    return `${cores} ${cores === 1 ? 'core' : 'cores'}`
  }
  return null
}

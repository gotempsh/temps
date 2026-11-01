// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { MetricSparkline } from '@/components/charts/metric-sparkline'
import { cn } from '@/lib/utils'
import {
  buildMetricHistorySeries,
  hasMetricHistory,
} from './container-metric-history'
import { useContainerMetricHistory } from './use-container-metric-history'

interface ContainerMetricHistoryProps {
  projectId: number
  environmentId: number
  containerId: string
  metric: string
  label: string
  format: (value: number) => string
  currentValue?: number | null
  hideWithoutHistory?: boolean
  enabled?: boolean
  className?: string
}

/** Compact one-hour resource sparkline with its latest value. */
export function ContainerMetricHistory({
  projectId,
  environmentId,
  containerId,
  metric,
  label,
  format,
  currentValue,
  hideWithoutHistory = false,
  enabled = true,
  className,
}: ContainerMetricHistoryProps) {
  const { data } = useContainerMetricHistory({
    projectId,
    environmentId,
    containerId,
    metric,
    enabled,
  })

  if (!enabled) return null
  if (hideWithoutHistory && !hasMetricHistory(data)) return null

  const values = buildMetricHistorySeries(data, currentValue)
  const latest = values[values.length - 1]

  return (
    <div
      className={cn(
        'flex w-24 shrink-0 flex-col items-stretch gap-0.5',
        className
      )}
      role="img"
      aria-label={`${label} usage over the last hour`}
    >
      <MetricSparkline data={values} height={16} />
      <span className="text-right text-[10px] tabular-nums text-neutral-600 dark:text-neutral-400">
        {label} {latest == null ? 'No data' : format(latest)}
      </span>
    </div>
  )
}

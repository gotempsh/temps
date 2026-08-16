import { containerMetricsGetHistoryOptions } from '@/api/client/@tanstack/react-query.gen'
import { MetricSparkline } from '@/components/charts/metric-sparkline'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { buildMetricHistorySeries } from './container-metric-history'

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
  const { data } = useQuery({
    ...containerMetricsGetHistoryOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
        container_id: containerId,
      },
      query: { metric, range: '1h' },
    }),
    staleTime: 30_000,
    refetchInterval: 30_000,
    enabled,
    // Metrics store disabled -> the endpoint returns 503; avoid retry spam.
    retry: false,
  })

  if (!enabled) return null
  if (hideWithoutHistory && !data?.length) return null

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
      <span className="text-right text-[10px] tabular-nums text-neutral-500 dark:text-neutral-400">
        {label} {latest == null ? 'No data' : format(latest)}
      </span>
    </div>
  )
}

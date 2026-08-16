interface MetricHistoryPoint {
  value: number
}

/**
 * Build a sparkline series from stored history and the latest live sample.
 * The live sample keeps the right edge current between 30-second history
 * refreshes and still gives the UI a baseline when history is unavailable.
 */
export function buildMetricHistorySeries(
  history: MetricHistoryPoint[] | undefined,
  currentValue?: number | null
): number[] {
  const values = (history ?? [])
    .map((point) => point.value)
    .filter(Number.isFinite)

  if (currentValue != null && Number.isFinite(currentValue)) {
    values.push(currentValue)
  }

  return values
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

interface MetricHistoryPoint {
  /** `null` for a bucket the container reported no sample in. */
  value?: number | null
}

/** Whether any bucket of the history holds a sample. */
export function hasMetricHistory(
  history: MetricHistoryPoint[] | undefined
): boolean {
  return (history ?? []).some(
    (point) => point.value != null && Number.isFinite(point.value)
  )
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
  // Trend glyph only: buckets without a sample are dropped rather than drawn.
  const values = (history ?? [])
    .map((point) => point.value)
    .filter((value): value is number => value != null && Number.isFinite(value))

  if (currentValue != null && Number.isFinite(currentValue)) {
    values.push(currentValue)
  }

  return values
}

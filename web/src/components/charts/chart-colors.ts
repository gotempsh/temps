// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MetricTone } from './metric-sparkline'

/** Stroke used for a single-tone line, keyed by its semantic tone. */
export const SERIES_STROKE: Record<MetricTone | 'primary', string> = {
  good: 'var(--chart-2)',
  warn: 'var(--chart-3)',
  poor: 'var(--chart-4)',
  neutral: 'var(--chart-1)',
  primary: 'var(--chart-1)',
}

// Breakdown lines don't carry good/warn/poor semantics — cycle the same five
// theme chart vars every other chart in the app rotates through (see
// AiAgentsTimelineChart's SERIES_COLORS) so a breakdown looks native in both
// light and dark mode instead of inventing new hex colors.
export const BREAKDOWN_STROKES = [
  'var(--chart-1)',
  'var(--chart-2)',
  'var(--chart-3)',
  'var(--chart-4)',
  'var(--chart-5)',
]

export const THRESHOLD_STROKE: Record<MetricTone, string> = {
  good: 'var(--chart-2)',
  warn: 'var(--chart-3)',
  poor: 'var(--chart-4)',
  neutral: 'var(--muted-foreground)',
}

/**
 * Resolve the stroke color a multi-series `ThresholdLineChart` picks for a
 * given `tone` and position — for callers that render a legend or swatch
 * *outside* the chart (e.g. a caption row under the card) and need it to
 * match the chart's own line colors. Callers should read this instead of
 * hand-copying `SERIES_STROKE`/`BREAKDOWN_STROKES`, which would silently
 * drift from the chart's own colors if this table ever changes.
 */
export function seriesLineColor(
  tone: MetricTone | 'primary' | undefined,
  index: number
): string {
  return tone
    ? SERIES_STROKE[tone]
    : BREAKDOWN_STROKES[index % BREAKDOWN_STROKES.length]
}

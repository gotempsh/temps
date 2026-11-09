// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MetricTone } from '@/components/charts/metric-sparkline'

/**
 * How a service's HTTP error-rate line reads. Red only when something
 * failed: a line drawn in the alarm colour along 0% tells a healthy (or
 * brand-new) service that it is in trouble.
 */
export function errorRateTone(
  overallErrorRate: number | undefined
): MetricTone {
  return (overallErrorRate ?? 0) > 0 ? 'poor' : 'neutral'
}

/**
 * Points worth plotting. With no requests in the window there is no rate to
 * draw, only a row of zeros, so the chart shows its empty state instead.
 */
export function errorRatePoints<T>(
  points: T[],
  totalRequests: number | undefined
): T[] {
  return (totalRequests ?? 0) > 0 ? points : []
}

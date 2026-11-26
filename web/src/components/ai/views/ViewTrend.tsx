// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { TimeChart } from '@temps-sdk/ds'

export interface ViewTrendPoint {
  at: string
  value: number
}

/**
 * One measure over time, on the console's own time chart: the same line,
 * axis, hover and "so far" treatment of a period in progress as every other
 * time series in the console. There is one series, so the caption names it
 * and no legend is drawn.
 */
export default function ViewTrend({
  label,
  points,
  partial,
}: {
  label: string
  points: ViewTrendPoint[]
  partial?: boolean | null
}) {
  return (
    <figure className="space-y-2 rounded-md border p-3">
      <figcaption className="text-sm font-medium">{label}</figcaption>
      <TimeChart
        data={points}
        xKey="at"
        height={200}
        series={{ dataKey: 'value', label, tone: 'primary' }}
        allowDecimals={points.some((point) => !Number.isInteger(point.value))}
        tooltipValueFormatter={(value) => value.toLocaleString()}
        lastPointPartial={partial === true}
        emptyMessage="Not enough points to draw a line"
      />
    </figure>
  )
}

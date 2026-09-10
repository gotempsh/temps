// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Line, LineChart, ResponsiveContainer, YAxis } from 'recharts'
import { cn } from '@/lib/utils'

export type MetricTone = 'good' | 'warn' | 'poor' | 'neutral'

const TONE_STROKE: Record<MetricTone, string> = {
  good: 'var(--chart-2)',
  warn: 'var(--chart-3)',
  poor: 'var(--chart-4)',
  neutral: 'var(--muted-foreground)',
}

interface MetricSparklineProps {
  data: (number | null | undefined)[]
  tone?: MetricTone
  className?: string
  height?: number
}

/**
 * Small inline sparkline for metric tiles. Uses theme tokens so the stroke
 * adapts to light/dark automatically. Renders a flat baseline when there's
 * not enough data to draw a line.
 */
export function MetricSparkline({
  data,
  tone = 'neutral',
  className,
  height = 40,
}: MetricSparklineProps) {
  const points = data.map((v, i) => ({ i, v: v ?? null }))
  const valid = points.filter((p) => p.v !== null)

  if (valid.length < 2) {
    return (
      <div
        className={cn('flex w-full items-center', className)}
        style={{ height }}
      >
        <div className="h-px w-full bg-border" />
      </div>
    )
  }

  return (
    <div className={cn('w-full', className)} style={{ height }}>
      <ResponsiveContainer width="100%" height="100%">
        <LineChart data={points} margin={{ top: 2, right: 0, left: 0, bottom: 2 }}>
          <YAxis hide domain={['dataMin', 'dataMax']} />
          <Line
            type="monotone"
            dataKey="v"
            stroke={TONE_STROKE[tone]}
            strokeWidth={1.5}
            dot={false}
            connectNulls
            isAnimationActive={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  )
}

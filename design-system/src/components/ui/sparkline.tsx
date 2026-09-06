// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'

interface SparklineProps {
  values: number[]
  /** Pixel height. Width is one bar + one gap per bucket. */
  height?: number
  /** Which bar to emphasise; defaults to the last one. */
  highlight?: number
  className?: string
  label?: string
}

/**
 * Bar-per-bucket sparkline for metric tiles. Foreground-coloured bars on
 * transparent, no axis, no curve — a terminal-style trend that survives at
 * 20px tall next to a number without needing a chart library. Only the
 * highlighted (latest) bar is fully opaque so the eye lands on "now".
 */
export function Sparkline({
  values,
  height = 20,
  highlight,
  className,
  label,
}: SparklineProps) {
  const max = Math.max(1, ...values)
  const bar = 3
  const gap = 1
  const width = values.length * (bar + gap) - gap
  const hi = highlight ?? values.length - 1
  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label={label ?? `Trend over ${values.length} points`}
      className={cn('shrink-0 fill-foreground', className)}
    >
      {values.map((v, i) => {
        const h = Math.max(1, Math.round((v / max) * height))
        return (
          <rect
            key={i}
            x={i * (bar + gap)}
            y={height - h}
            width={bar}
            height={h}
            opacity={i === hi ? 1 : 0.35}
          />
        )
      })}
    </svg>
  )
}

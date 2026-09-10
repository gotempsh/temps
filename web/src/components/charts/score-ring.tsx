// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import type { MetricTone } from './metric-sparkline'

interface ScoreRingProps {
  /** 0–100 value to plot. */
  score: number
  /** Tone selects the ring color. */
  tone: MetricTone
  /** Diameter in px. Defaults to 72. */
  size?: number
  /** Stroke width in px. Defaults to 6. */
  strokeWidth?: number
  className?: string
}

const TONE_STROKE: Record<MetricTone, string> = {
  good: 'var(--chart-2)',
  warn: 'var(--chart-3)',
  poor: 'var(--chart-4)',
  neutral: 'var(--chart-1)',
}

const TONE_TEXT: Record<MetricTone, string> = {
  good: 'text-emerald-600 dark:text-emerald-400',
  warn: 'text-amber-600 dark:text-amber-400',
  poor: 'text-red-600 dark:text-red-400',
  neutral: 'text-foreground',
}

/**
 * Themed conic-style score ring. Used for overall scores (0–100) where the
 * fill angle maps directly to the score. Color follows the semantic tone so
 * it works on light and dark backgrounds.
 */
export function ScoreRing({
  score,
  tone,
  size = 72,
  strokeWidth = 6,
  className,
}: ScoreRingProps) {
  const radius = (size - strokeWidth) / 2
  const circumference = 2 * Math.PI * radius
  const clamped = Math.max(0, Math.min(100, score))
  const offset = circumference - (clamped / 100) * circumference

  return (
    <div
      className={cn('relative shrink-0', className)}
      style={{ width: size, height: size }}
    >
      <svg width={size} height={size} className="-rotate-90">
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          className="stroke-muted"
          strokeWidth={strokeWidth}
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke={TONE_STROKE[tone]}
          strokeWidth={strokeWidth}
          strokeDasharray={circumference}
          strokeDashoffset={offset}
          strokeLinecap="round"
          style={{ transition: 'stroke-dashoffset 400ms ease' }}
        />
      </svg>
      <div className="absolute inset-0 flex items-center justify-center">
        <span
          className={cn('text-xl font-semibold tabular-nums', TONE_TEXT[tone])}
        >
          {Math.round(clamped)}
        </span>
      </div>
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from './lib/cn'
import { EMPTY, fmtNum } from './fmt'
import { GLYPH, GLYPH_CLASS, type State } from './status'

/**
 * A number the operator will compare. Mono, tabular, unit after the value in
 * muted. Nothing is rendered as an en dash (–); zero is "0".
 */
export function Num({ value, unit, className }: { value: number | string | null | undefined; unit?: string; className?: string }) {
  if (value === null || value === undefined || value === '') {
    return <span className={cn('font-mono tabular-nums text-muted-foreground', className)}>{EMPTY}</span>
  }
  return (
    <span className={cn('font-mono tabular-nums', className)}>
      {typeof value === 'number' ? fmtNum(value) : value}
      {unit && <span className="text-muted-foreground">{unit}</span>}
    </span>
  )
}

/**
 * Metric tile. `baseline` is required on purpose: a delta with no baseline
 * ("+9%") is a number pretending to mean something. Write the comparison
 * ("since dep_91a", "vs yesterday", "90d window").
 * Tiles sit in a bordered grid; the grid draws the dividers, not the tile.
 */
export function Metric({ label, value, unit, delta, baseline, state = 'ok', className }: {
  label: string
  value: string | number
  unit?: string
  delta?: string
  baseline: string
  state?: State
  className?: string
}) {
  return (
    <div className={cn('p-3', className)}>
      <p className="op-label">{label}</p>
      <p className="mt-1 text-lg"><Num value={value} unit={unit} /></p>
      <p className={cn('text-[11px]', state === 'warn' ? 'text-warning' : state === 'error' ? 'text-destructive' : 'text-muted-foreground')}>
        {state !== 'ok' && <span aria-hidden className={cn('mr-1', GLYPH_CLASS[state])}>{GLYPH[state]}</span>}
        {delta ? `${delta} ` : ''}{baseline}
      </p>
    </div>
  )
}

/** Bordered grid for Metric tiles: 2 columns on phones, `cols` from md up. */
export function MetricGrid({ children, cols = 4, className }: { children: React.ReactNode; cols?: 2 | 3 | 4; className?: string }) {
  return (
    <div
      className={cn('op-metric-grid grid grid-cols-2 border', className)}
      style={{ '--metric-cols': cols } as React.CSSProperties}
    >
      {children}
    </div>
  )
}

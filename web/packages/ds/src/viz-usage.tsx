// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from './lib/cn'
import { fmtNum, fmtPct } from './fmt'
import { GLYPH, GLYPH_CLASS, type State } from './status'
import { Num } from './num'

/* ────────────────────────────────────────────────────────────────────────
   A number against a limit. Both forms here are text first: the sentence
   states the fact and the bar is the shape of it, never the other way
   round. No radial gauges — an arc cannot be compared with the arc beside
   it, and its ends are not a baseline.
   See `design-system/docs/data-viz.md`.
   ──────────────────────────────────────────────────────────────────────── */

// ── UsageBar ───────────────────────────────────────────────────────────

/**
 * Usage against an allowance: ingest, disk, bandwidth, AI credits, seats.
 *
 * Text first — the sentence above the bar states used, allowance and the plan
 * word, so the fact survives with the bar switched off. The bar adds two
 * things a number cannot: the overage past the allowance, hatched so it reads
 * as "beyond the line" rather than as more of the same, and the point where
 * sampling started, because everything after it is an estimate.
 *
 * The allowance is the bar's full width. Over it, the whole bar is the used
 * amount and the allowance is a rule inside it; that keeps the overage
 * legible instead of pinning the bar at 100%.
 *
 * ```tsx
 * <UsageBar label="events" used={8_420_000} allowance={10_000_000} unit="events"
 *   plan="Cloud Pro · 10M / month" sampledFrom={7_500_000}
 *   resets="resets 1 Oct" action={<a href="/settings/plan">change plan</a>} />
 * ```
 */
export function UsageBar({ label, used, allowance, unit = '', plan, format = (n) => fmtNum(n), sampledFrom, sampledLabel = 'sampled past here', resets, warnAt = 80, action, className }: {
  /** What is being counted ("events", "disk", "seats"). */
  label: string
  used: number
  /** The limit. Zero or below means "no limit" and the bar is not drawn. */
  allowance: number
  unit?: string
  /** The plan and its allowance in words ("Cloud Pro · 10M events / month"). Always shown: the reader must know which limit this is. */
  plan: string
  /** How to print the two numbers. Use `fmtBytes` for disk and bandwidth. */
  format?: (n: number) => string
  /** Where head sampling began. Everything past it is an estimate and the footer says so. */
  sampledFrom?: number
  sampledLabel?: string
  /** When the counter goes back to zero ("resets 1 Oct"). */
  resets?: ReactNode
  /** Share of the allowance at which the figure becomes a `warn` state. */
  warnAt?: number
  /** What the operator does about it — raise the limit, change the plan. */
  action?: ReactNode
  className?: string
}) {
  const share = allowance > 0 ? (used / allowance) * 100 : 0
  const over = Math.max(0, used - allowance)
  const state: State = allowance <= 0 ? 'idle' : over > 0 ? 'error' : share >= warnAt ? 'warn' : 'ok'
  const full = Math.max(allowance, used) || 1
  const pctOf = (n: number) => (n / full) * 100
  const word = over > 0 ? `${format(over)}${unit ? ` ${unit}` : ''} over` : `${format(Math.max(0, allowance - used))}${unit ? ` ${unit}` : ''} left`
  const sentence = `${label}: ${format(used)}${unit ? ` ${unit}` : ''} of ${format(allowance)}, ${fmtPct(share, { digits: share < 10 ? 1 : 0 })} of the allowance on ${plan}. ${word}.${sampledFrom ? ` Sampled past ${format(sampledFrom)}; the figure is an estimate.` : ''}`
  return (
    <div className={cn('min-w-0 border bg-background p-3 text-xs', className)}>
      {/* The fact, in words, before any picture of it. */}
      <p className="flex flex-wrap items-baseline gap-x-2">
        <span className="op-label">{label}</span>
        <span className="font-mono text-base tabular-nums">{format(used)}<span className="text-muted-foreground">{unit ? ` ${unit}` : ''}</span></span>
        <span className="font-mono text-[11px] text-muted-foreground">of {format(allowance)}{unit ? ` ${unit}` : ''} · {fmtPct(share, { digits: share < 10 ? 1 : 0 })}</span>
        <span className={cn('ml-auto font-mono text-[11px]', state === 'ok' ? 'text-muted-foreground' : GLYPH_CLASS[state])}>
          {state !== 'ok' && state !== 'idle' && <span aria-hidden className="mr-1">{GLYPH[state]}</span>}{word}
        </span>
      </p>
      {allowance > 0 && (
        <div role="img" aria-label={sentence} className="relative mt-2 h-4 border">
          <span aria-hidden className="absolute inset-y-0 left-0 bg-foreground" style={{ width: `${Math.min(100, pctOf(Math.min(used, allowance)))}%`, opacity: 0.8 }} />
          {/* Over the line is a different fact, so it is a different fill, not more of the same. */}
          {over > 0 && (
            <span aria-hidden className="op-ink-hatch-error absolute inset-y-0 border-l border-destructive"
              style={{ left: `${pctOf(allowance)}%`, width: `${pctOf(over)}%` }} />
          )}
          {sampledFrom !== undefined && sampledFrom < used && (
            <span aria-hidden className="absolute inset-y-0 w-px bg-muted-foreground" style={{ left: `${pctOf(sampledFrom)}%` }} />
          )}
        </div>
      )}
      <p className="mt-1.5 flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">
        <span>{plan}</span>
        {resets && <span>· {resets}</span>}
        {sampledFrom !== undefined && sampledFrom < used && <span>· ◌ {sampledLabel}, from {format(sampledFrom)}{unit ? ` ${unit}` : ''} · the figure past it is an estimate</span>}
        {over > 0 && <span className="text-destructive">· ▨ {format(over)}{unit ? ` ${unit}` : ''} over the allowance</span>}
        {action && <span className="ml-auto">{action}</span>}
      </p>
    </div>
  )
}

// ── Gauge ──────────────────────────────────────────────────────────────

/**
 * A resource on a machine — cpu, memory, disk — as a `MetricGrid`-shaped tile:
 * the current figure, a horizontal ink bar from zero, the thresholds as ticks
 * that carry their own words, and the peak in the window so a flat-looking
 * average cannot hide a spike.
 *
 * Horizontal and linear on purpose. A radial gauge cannot be compared with
 * the one beside it, its scale has no baseline, and its needle is a picture of
 * one number that the number itself already gives.
 *
 * ```tsx
 * <MetricGrid cols={3}>
 *   <Gauge label="memory" value={91} of="of 4 GB" peak={94} peakLabel="peak 20:41"
 *     thresholds={[{ at: 80, state: 'warn', label: 'warn' }, { at: 95, state: 'error', label: 'stop' }]} />
 * </MetricGrid>
 * ```
 */
export function Gauge({ label, value, max = 100, unit = '%', of, peak, peakLabel, thresholds = [], idle, className }: {
  label: string
  /** The current reading. */
  value: number
  /** Full scale. The bar starts at zero and ends here — never truncated. */
  max?: number
  unit?: string
  /** What the scale is of ("of 4 GB", "of 3 vCPU"). Printed under the figure. */
  of?: ReactNode
  /** Highest reading in the window. Drawn as a tick and printed beside the figure. */
  peak?: number
  /** When or over what window the peak was reached ("at 20:41", "in 6h"). Required whenever `peak` is set: a peak with no window is a number with no meaning. */
  peakLabel?: string
  /** Warn and error lines. Each is a tick and a word — never a colour on its own. */
  thresholds?: { at: number; state: 'warn' | 'error'; label: string }[]
  /** No samples (the node is offline). The tile stays, the bar is empty, and the words say why. */
  idle?: ReactNode
  className?: string
}) {
  const crossed = [...thresholds].sort((a, b) => b.at - a.at).find((t) => value >= t.at)
  const state: State = idle ? 'idle' : crossed ? crossed.state : 'ok'
  const pct = (n: number) => Math.max(0, Math.min(100, (n / Math.max(1e-9, max)) * 100))
  const sentence = `${label} ${fmtNum(value)}${unit}${of ? ` ${typeof of === 'string' ? of : ''}` : ''}${peak !== undefined ? `, peak ${fmtNum(peak)}${unit} ${peakLabel ?? ''}` : ''}${crossed ? `, above the ${crossed.label} line at ${fmtNum(crossed.at)}${unit}` : ''}.`
  return (
    <div className={cn('min-w-0 p-3', className)}>
      <p className="op-label truncate">{label}</p>
      <p className={cn('mt-1 flex items-baseline gap-2 text-lg leading-6', idle && 'text-muted-foreground')}>
        <Num value={idle ? null : fmtNum(value)} unit={unit} />
        {crossed && !idle && <span className={cn('font-mono text-[11px]', GLYPH_CLASS[crossed.state])}><span aria-hidden>{GLYPH[crossed.state]}</span> {crossed.label}</span>}
      </p>
      <div role="img" aria-label={idle ? `${label}: no samples` : sentence} className="relative mt-1.5 h-2 border">
        {!idle && <span aria-hidden className="absolute inset-y-0 left-0 bg-foreground" style={{ width: `${pct(value)}%`, opacity: 0.8 }} />}
        {thresholds.map((t) => (
          <span key={t.label} aria-hidden className={cn('absolute -top-0.5 -bottom-0.5 w-px', t.state === 'error' ? 'bg-destructive' : 'bg-warning')} style={{ left: `${pct(t.at)}%` }} />
        ))}
        {peak !== undefined && !idle && <span aria-hidden className="absolute -top-1 -bottom-1 w-px bg-foreground" style={{ left: `${pct(peak)}%` }} />}
      </div>
      <p className={cn('mt-1 font-mono text-[11px]', state === 'ok' || state === 'idle' ? 'text-muted-foreground' : GLYPH_CLASS[state])}>
        {idle ?? <>{of}{peak !== undefined && <> · peak {fmtNum(peak)}{unit}{peakLabel ? ` ${peakLabel}` : ''}</>}</>}
      </p>
      {/* Every tick says its own word: a line the reader cannot name is decoration. */}
      {!idle && thresholds.length > 0 && (
        <p className="font-mono text-[10px] text-muted-foreground">{thresholds.map((t) => `┆ ${t.label} ${fmtNum(t.at)}${unit}`).join(' · ')}</p>
      )}
    </div>
  )
}

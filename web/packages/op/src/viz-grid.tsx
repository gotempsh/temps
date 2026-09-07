// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { cn } from './lib/cn'
import { fmtNum, fmtPct } from './fmt'
import { GLYPH, GLYPH_CLASS, type State } from './status'
import { Num } from './num'
import { INK_STEPS, ReadoutLive, inkCell, inkStep } from './viz-ink'

/* ────────────────────────────────────────────────────────────────────────
   Figures that are grids of numbers: a ladder of percentiles, retention
   cohorts, a before/after comparison. Where the thing genuinely is a table
   it is drawn as one — `<table>` with real headers — and the ink only says
   how much. See `design-system/docs/data-viz.md`.
   ──────────────────────────────────────────────────────────────────────── */

// ── PercentileLadder ───────────────────────────────────────────────────

/** One rung: the statistic, its value, and what the delta beside it is measured against. */
export type Rung = {
  /** `p50`, `p95`, `p99`, `max`. Printed as given. */
  name: string
  value: number
  /** Signed change, already computed ("+18%", "−4 ms"). Optional. */
  delta?: string
  /** What the delta is measured against. Required whenever `delta` is set: a delta with no baseline is a rumour. */
  baseline?: string
  /** Only when the rung is itself a state — p99 over its budget. */
  state?: 'ok' | 'warn' | 'error'
}

/**
 * p50 · p95 · p99 · max as one compact figure: the name, the number, an ink
 * bar for the shape, and each rung's own delta with its baseline. It is the
 * aside and tile form of what a `Histogram` says at full size — four numbers,
 * so it is a small table of numbers and not a chart of them.
 *
 * The bars share one scale from zero, so the reader sees the tail: a p99 four
 * times p50 is four times the bar. Tone lands only on a rung that is a state.
 *
 * ```tsx
 * <PercentileLadder unit="ms" label="checkout latency"
 *   rungs={[{ name: 'p50', value: 41 },
 *           { name: 'p99', value: 402, delta: '+18%', baseline: 'vs prior 24h', state: 'warn' }]} />
 * ```
 */
export function PercentileLadder({ rungs, unit = 'ms', label, meta, className }: {
  rungs: Rung[]
  unit?: string
  /** What the figure is of ("checkout latency"). Goes into the `aria-label`. */
  label: string
  /** One line under the ladder: the window, the sample count. */
  meta?: ReactNode
  className?: string
}) {
  const max = Math.max(1, ...rungs.map((r) => r.value))
  const sentence = `${label} in ${unit}: ${rungs.map((r) => `${r.name} ${fmtNum(r.value)}${r.delta ? `, ${r.delta} ${r.baseline ?? ''}` : ''}`).join('; ')}.`
  return (
    <div className={cn('min-w-0 border bg-background', className)}>
      {/* An `.op-rows` list, not a `<dl>`: each rung carries a bar and two
          numbers, which is a row, and a definition list whose items are wrapped
          divs with a bar in them is not a definition list any more. The
          `role="img"` sentence sits on the wrapper, because putting it on the
          `<ol>` would strip the list semantics from every rung. */}
      <div role="img" aria-label={sentence}>
      <ol className="op-rows text-xs">
        {rungs.map((r) => (
          <li key={r.name} className="relative grid grid-cols-[2.5rem_minmax(0,1fr)_auto] items-center gap-3 px-3 py-1.5">
            <span aria-hidden className="absolute inset-y-1 left-0 bg-foreground/[0.06]" style={{ width: `${(r.value / max) * 100}%` }} />
            <span className="relative op-label">{r.name}</span>
            <span className="relative min-w-0">
              <Num value={r.value} unit={unit} />
              {r.state && r.state !== 'ok' && <span className={cn('ml-2 text-[11px]', GLYPH_CLASS[r.state])}><span aria-hidden>{GLYPH[r.state]}</span> over budget</span>}
            </span>
            {/* A delta never appears without the window it is measured against. */}
            <span className="relative text-right font-mono text-[11px] text-muted-foreground">
              {r.delta ? <><span className="text-foreground">{r.delta}</span> {r.baseline}</> : ''}
            </span>
          </li>
        ))}
      </ol>
      </div>
      {meta && <p className="border-t px-3 py-1.5 font-mono text-[10px] text-muted-foreground">{meta}</p>}
    </div>
  )
}

// ── CohortGrid ─────────────────────────────────────────────────────────

/** One cohort: who joined when, how many, and what share came back each period. */
export type Cohort = {
  /** What the cohort is called ("Aug 25", "week of 2026-08-25"). */
  label: string
  /** How many entered the cohort. The denominator of every cell. */
  size: number
  /**
   * Share still active in period 0, 1, 2 … as percentages on the 0–100 scale.
   * A shorter array is an honest cohort that has not lived that long yet: the
   * missing cells are drawn empty, never as zero.
   */
  values: number[]
}

/**
 * Retention cohorts. This is a table and is drawn as one — `<table>` with a
 * row header per cohort and a column header per period — because every cell
 * has two coordinates and a reader needs both read aloud. The ink says how
 * much; the number is in the cell, so the density is never the only encoding.
 *
 * A cell a cohort has not reached yet is empty, not zero: "nobody came back"
 * and "not yet known" are different facts.
 *
 * ```tsx
 * <CohortGrid cohorts={weeks} periodLabel="week" label="signup retention"
 *   verdict="Week 1 holds 41% and flattens at 22% from week 4." />
 * ```
 */
export function CohortGrid({ cohorts, periodLabel = 'period', label, verdict, meta, className }: {
  cohorts: Cohort[]
  /** What one column is ("week", "day", "month"). Column heads read `week 0`, `week 1` … */
  periodLabel?: string
  label: string
  verdict: string
  meta?: ReactNode
  className?: string
}) {
  const periods = Math.max(0, ...cohorts.map((c) => c.values.length))
  const [at, setAt] = useState<{ r: number; c: number } | null>(null)
  const cell = at ? cohorts[at.r].values[at.c] : undefined
  const read = at && cell !== undefined
    ? `${cohorts[at.r].label} · ${periodLabel} ${at.c} · ${fmtPct(cell)} of ${fmtNum(cohorts[at.r].size)}`
    : at ? `${cohorts[at.r].label} · ${periodLabel} ${at.c} · not reached yet` : ''
  const move = (dr: number, dc: number) => setAt((p) => {
    const n = p ?? { r: 0, c: 0 }
    return { r: Math.max(0, Math.min(cohorts.length - 1, n.r + dr)), c: Math.max(0, Math.min(periods - 1, n.c + dc)) }
  })
  const sentence = `${label}, ${cohorts.length} cohorts by ${periods} ${periodLabel}s, ink density is the share retained. ${verdict.replace(/\.\s*$/, '')}.`
  return (
    <div className={cn('min-w-0 border bg-background', className)}>
      <div
        role="group" tabIndex={0} aria-label={`${sentence} Use arrow keys to read a cell.`}
        onFocus={() => setAt((p) => p ?? { r: 0, c: 0 })} onBlur={() => setAt(null)}
        onKeyDown={(e) => { const k = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[e.key]; if (k) { e.preventDefault(); move(k[0], k[1]) } }}
        className="min-w-0 overflow-auto outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
      >
        <table className="w-full font-mono text-[11px]">
          <caption className="op-label border-b px-3 py-1.5 text-left text-[9px]">{label} · {read || `${fmtNum(cohorts.reduce((a, c) => a + c.size, 0))} in ${cohorts.length} cohorts`}</caption>
          <thead>
            <tr>
              <th scope="col" className="op-label border-b px-2 py-1 text-left text-[9px]">cohort</th>
              <th scope="col" className="op-label border-b px-2 py-1 text-right text-[9px]">size</th>
              {Array.from({ length: periods }, (_, c) => <th key={c} scope="col" className="op-label border-b px-2 py-1 text-right text-[9px]">{periodLabel} {c}</th>)}
            </tr>
          </thead>
          <tbody className="op-rows">
            {cohorts.map((co, r) => (
              <tr key={co.label}>
                <th scope="row" className="whitespace-nowrap px-2 py-1 text-left font-normal text-muted-foreground">{co.label}</th>
                <td className="px-2 py-1 text-right tabular-nums text-muted-foreground">{fmtNum(co.size)}</td>
                {Array.from({ length: periods }, (_, c) => {
                  const v = co.values[c]
                  return (
                    <td key={c} onMouseEnter={() => setAt({ r, c })} onMouseLeave={() => setAt(null)}
                      className={cn('relative px-2 py-1 text-right tabular-nums', at && at.r === r && at.c === c && 'outline outline-1 -outline-offset-1 outline-foreground')}>
                      {v === undefined ? <span className="text-muted-foreground">–</span> : <>
                        <span aria-hidden className="absolute inset-0" style={inkCell(v, 100)} />
                        {/* The wash is black on both layers, so a dense cell needs the
                            colour that reads on black — `--background` on paper, ink on
                            night. Flipping at the top step only left the 0.42 middle of
                            the ladder as a 50% grey that neither end could sit on. */}
                        <span className="relative" style={inkStep(v, 100) >= 3 ? { color: 'var(--op-ink-on-dense)' } : undefined}>{fmtPct(v, { digits: 0 })}</span>
                      </>}
                    </td>
                  )
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-t px-3 py-1.5 font-mono text-[10px] text-muted-foreground">
        <span className="flex items-center gap-1">0% {INK_STEPS.map((o, s) => <span key={s} className="block h-2 w-2" style={{ backgroundColor: 'var(--foreground)', opacity: o }} />)} 100%</span>
        <span>{meta ?? '– not reached yet'}</span>
      </div>
      <ReadoutLive text={read} />
    </div>
  )
}

// ── DeltaTable ─────────────────────────────────────────────────────────

/** One metric compared across two releases, windows or nodes. */
export type DeltaRow = {
  metric: string
  before: number
  after: number
  unit?: string
  /** `lower` when a smaller number is better (latency, errors). Decides the wording, never the tone. */
  better?: 'lower' | 'higher'
  /**
   * The line that makes the "after" value a state. Tone appears only when the
   * after value is on the wrong side of it — a delta is not a state by itself.
   */
  threshold?: { at: number; state: 'warn' | 'error'; label: string }
  note?: string
}

/**
 * Release comparison: metric · before · after · delta. The deltas are `Num`s,
 * ink like every other number, and take a tone **only** when a threshold makes
 * the after value a state. A red "+12%" on a metric with no budget is a colour
 * with no meaning: the reader cannot tell whether it is bad or merely bigger.
 *
 * ```tsx
 * <DeltaTable before="dep_91a" after="dep_91b" rows={[
 *   { metric: 'p95 latency', before: 402, after: 188, unit: 'ms', better: 'lower' },
 *   { metric: '5xx rate', before: 0.02, after: 1.4, unit: '%', better: 'lower',
 *     threshold: { at: 1, state: 'error', label: 'budget 1%' } }]} />
 * ```
 */
export function DeltaTable({ rows, before, after, label, meta, className }: {
  rows: DeltaRow[]
  /** What the "before" column is ("dep_91a", "previous 7d"). */
  before: string
  /** What the "after" column is. */
  after: string
  /** What the comparison is of. Defaults to "<after> vs <before>". */
  label?: string
  meta?: ReactNode
  className?: string
}) {
  const pct = (r: DeltaRow) => (r.before ? ((r.after - r.before) / Math.abs(r.before)) * 100 : undefined)
  const toneOf = (r: DeltaRow): State | undefined => {
    if (!r.threshold) return undefined
    const over = r.better === 'higher' ? r.after < r.threshold.at : r.after > r.threshold.at
    return over ? r.threshold.state : undefined
  }
  const caption = label ?? `${after} vs ${before}`
  return (
    <div className={cn('min-w-0 border bg-background', className)}>
      {/* Five columns of numbers do not fit a phone: the table scrolls inside
          its own frame rather than pushing the page sideways. */}
      <div className="min-w-0 overflow-auto">
      <table className="w-full min-w-[22rem] text-xs">
        <caption className="op-label border-b px-3 py-1.5 text-left text-[9px]">{caption}</caption>
        <thead>
          <tr>
            <th scope="col" className="op-label border-b px-3 py-1 text-left text-[9px]">metric</th>
            <th scope="col" className="op-label border-b px-3 py-1 text-right text-[9px]">{before}</th>
            <th scope="col" className="op-label border-b px-3 py-1 text-right text-[9px]">{after}</th>
            <th scope="col" className="op-label border-b px-3 py-1 text-right text-[9px]">delta</th>
          </tr>
        </thead>
        <tbody className="op-rows">
          {rows.map((r) => {
            const d = pct(r)
            const tone = toneOf(r)
            return (
              <tr key={r.metric}>
                <th scope="row" className="px-3 py-1.5 text-left font-normal">
                  {r.metric}
                  {r.note && <span className="ml-2 text-[11px] text-muted-foreground">{r.note}</span>}
                </th>
                <td className="px-3 py-1.5 text-right"><Num value={r.before} unit={r.unit} className="text-muted-foreground" /></td>
                <td className="px-3 py-1.5 text-right">
                  <Num value={r.after} unit={r.unit} />
                  {/* Tone arrives with a glyph and the threshold's own words, never alone. */}
                  {tone && <span className={cn('ml-2 font-mono text-[11px]', GLYPH_CLASS[tone])}><span aria-hidden>{GLYPH[tone]}</span> {r.threshold?.label}</span>}
                </td>
                <td className={cn('px-3 py-1.5 text-right font-mono tabular-nums', tone ? GLYPH_CLASS[tone] : '')}>
                  {d === undefined ? '–' : `${d >= 0 ? '+' : ''}${fmtNum(d, { digits: Math.abs(d) < 10 ? 1 : 0 })}%`}
                  <span className="ml-1 text-[11px] text-muted-foreground">{r.better === 'lower' ? (r.after <= r.before ? 'better' : 'worse') : r.better === 'higher' ? (r.after >= r.before ? 'better' : 'worse') : ''}</span>
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
      </div>
      <p className="border-t px-3 py-1.5 font-mono text-[10px] text-muted-foreground">{meta ?? <>every delta is {after} against {before}</>}</p>
    </div>
  )
}

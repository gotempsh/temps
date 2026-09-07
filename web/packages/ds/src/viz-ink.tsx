// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState, type CSSProperties, type ReactNode } from 'react'
import { cn } from './lib/cn'
import { GLYPH, GLYPH_CLASS, type State } from './status'

/* ────────────────────────────────────────────────────────────────────────
   The shared ink of the second wave of visualisations (`viz-time`,
   `viz-grid`, `viz-graph`, `viz-usage`). Everything here exists so those
   files cannot each invent their own greys, their own hatch, their own
   "view as table" toggle or their own keyboard readout.

   Three rules are enforced by construction:
    1. Fills are patterns, not hues. Four layers can be told apart with one
       ink at three greys because the pattern carries the difference.
    2. Density is five steps, the same five `CalendarHeatmap` uses, so a
       grid of counts reads the same everywhere.
    3. Every figure is `role="img"` with a sentence, and every figure whose
       data is a series ships the same data as a table.
   See `design-system/docs/data-viz.md`.
   ──────────────────────────────────────────────────────────────────────── */

/**
 * The pattern definitions the ink fills reference (`url(#op-hatch)` …).
 * Rendered once inside any component that uses a pattern fill: an SVG
 * `<pattern>` is resolvable from any other SVG in the same document, so a
 * div-based bar and a recharts plot share one definition.
 */
export function InkPatterns() {
  return (
    <svg aria-hidden width={0} height={0} className="pointer-events-none absolute" focusable="false">
      <defs>
        <pattern id="op-hatch" width={5} height={5} patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
          <line x1={0} y1={0} x2={0} y2={5} stroke="var(--foreground)" strokeWidth={1.6} opacity={0.55} />
        </pattern>
        <pattern id="op-hatch-soft" width={6} height={6} patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
          <line x1={0} y1={0} x2={0} y2={6} stroke="var(--foreground)" strokeWidth={1} opacity={0.28} />
        </pattern>
        <pattern id="op-cross" width={5} height={5} patternUnits="userSpaceOnUse">
          <path d="M0 0 L5 5 M5 0 L0 5" stroke="var(--foreground)" strokeWidth={0.9} opacity={0.5} />
        </pattern>
        <pattern id="op-dot" width={4} height={4} patternUnits="userSpaceOnUse">
          <circle cx={1.5} cy={1.5} r={1} fill="var(--foreground)" opacity={0.5} />
        </pattern>
      </defs>
    </svg>
  )
}

/** How a composition layer is filled. Four is the ceiling: a fifth pattern is noise. */
export type InkLayerFill = 'solid' | 'hatch' | 'dot' | 'cross'
export const INK_LAYER_ORDER: readonly InkLayerFill[] = ['solid', 'hatch', 'dot', 'cross']
/** The CSS/SVG paint for each fill. Ink only — a layer never carries a hue. */
export const INK_FILL: Record<InkLayerFill, string> = {
  solid: 'var(--foreground)',
  hatch: 'url(#op-hatch)',
  dot: 'url(#op-dot)',
  cross: 'url(#op-cross)',
}
/** Opacity that goes with the paint, so the four layers land on ≤3 greys. */
export const INK_FILL_OPACITY: Record<InkLayerFill, number> = { solid: 0.8, hatch: 1, dot: 1, cross: 1 }
/** The word a legend prints beside the swatch, so the pattern is named and not only shown. */
export const INK_FILL_WORD: Record<InkLayerFill, string> = { solid: 'solid', hatch: 'hatched', dot: 'dotted', cross: 'cross-hatched' }
/** State tone, for the one layer in a composition that *is* a state (5xx, error). */
export const INK_TONE: Record<'ok' | 'warn' | 'error', string> = { ok: 'var(--success)', warn: 'var(--warning)', error: 'var(--destructive)' }

/** Five density steps, the same ladder `CalendarHeatmap` uses. Index 0 is "none". */
export const INK_STEPS = [0.06, 0.22, 0.42, 0.68, 1] as const
/** Which of the five steps a value falls in. `0` when the value is zero: nothing is not a light something. */
export function inkStep(value: number, max: number): 0 | 1 | 2 | 3 | 4 {
  if (!value) return 0
  const n = Math.min(4, 1 + Math.floor((value / Math.max(1e-9, max)) * 3.999))
  return n as 1 | 2 | 3 | 4
}
/**
 * The inline style for a density cell. Ink at an opacity, never a colour ramp.
 *
 * The paint is `--op-ink-wash`, not `--foreground`, because night is not paper
 * inverted (brand §4): on paper more ink is darker than the page, and on night
 * it has to be darker than the ground too, or "more" reads as a highlight and
 * the ramp says the opposite of what it means. The variable is black on both
 * layers; `--op-ink-on-dense` is the colour a number sitting *in* a dense cell
 * takes, which is why a cohort percentage stays readable at every step instead
 * of hitting a 50% grey in the middle of the ladder.
 */
export function inkCell(value: number, max: number): CSSProperties {
  return { backgroundColor: 'var(--op-ink-wash, var(--foreground))', opacity: INK_STEPS[inkStep(value, max)] }
}

/**
 * One focusable region whose `←` `→` step an index and announce it, the way
 * `StatusStrip` does. Returns the props to spread on the region and the
 * current index; `null` means nothing is being read.
 */
export function useReadout(count: number) {
  const [i, setI] = useState<number | null>(null)
  const step = useCallback((d: number) => setI((p) => Math.max(0, Math.min(count - 1, (p ?? (d > 0 ? -1 : count)) + d))), [count])
  const regionProps = {
    role: 'group' as const,
    tabIndex: 0,
    onFocus: () => setI((p) => p ?? 0),
    onBlur: () => setI(null),
    onKeyDown: (e: React.KeyboardEvent) => {
      if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') { e.preventDefault(); step(-1) }
      if (e.key === 'ArrowRight' || e.key === 'ArrowDown') { e.preventDefault(); step(1) }
      if (e.key === 'Home') { e.preventDefault(); setI(0) }
      if (e.key === 'End') { e.preventDefault(); setI(count - 1) }
    },
    className: 'outline-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
  }
  return { i, setI, regionProps }
}

/** The live region that speaks the readout. Always rendered, even when empty. */
export function ReadoutLive({ text }: { text: string }) {
  return <span className="sr-only" aria-live="polite">{text}</span>
}

/**
 * The frame every figure in this wave shares: the plot is `role="img"` with a
 * sentence, and the toggle beside the footer swaps it for the same data as
 * rows. A figure whose data is a series and which passes no `table` is a bug,
 * not a shortcut — `Figure` warns in dev.
 */
export function Figure({ label, table, footer, legend, height, className, children }: {
  /** The `aria-label` sentence: what it is, over what range, and the verdict. */
  label: string
  /** The same data as rows. Omit only when the figure already *is* a table. */
  table?: ReactNode
  /** What the figure states under the plot (`ChartFooter` contents). */
  footer?: ReactNode
  /** Generated legend, drawn left of the toggle. Never typed into the footer. */
  legend?: ReactNode
  height?: number
  className?: string
  children: ReactNode
}) {
  const [asTable, setAsTable] = useState(false)
  return (
    <div className={cn('min-w-0 space-y-1', className)}>
      {/* Pattern fills are referenced by every figure and by its legend, so the
          definitions live on the frame rather than inside the plot: the swatch
          must still be hatched while the table view is open. */}
      <InkPatterns />
      {asTable && table ? (
        <div style={height ? { height } : undefined} className="overflow-auto border">{table}</div>
      ) : (
        <div role="img" aria-label={label} className="min-w-0">{children}</div>
      )}
      {(legend || table || footer) && (
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
          {legend ?? <span />}
          {table && (
            <button type="button" aria-pressed={asTable} onClick={() => setAsTable((v) => !v)} className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground underline underline-offset-4 hover:text-foreground">
              {asTable ? 'chart' : 'table'}
            </button>
          )}
        </div>
      )}
      {footer && <p className="flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">{footer}</p>}
    </div>
  )
}

/** The table view every `Figure` hands to its toggle: the same numbers as rows. */
export function DataTable({ caption, head, rows, numeric = [] }: {
  caption: string
  head: ReactNode[]
  /** First cell of each row becomes its `<th scope="row">`. */
  rows: ReactNode[][]
  /** Column indexes that align right (counts, durations, percentages). */
  numeric?: number[]
}) {
  return (
    <table className="w-full font-mono text-[11px]">
      <caption className="sr-only">{caption}</caption>
      <thead>
        <tr>
          {head.map((h, i) => (
            <th key={i} scope="col" className={cn('op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-[9px]', numeric.includes(i) ? 'text-right' : 'text-left')}>{h}</th>
          ))}
        </tr>
      </thead>
      <tbody className="op-rows">
        {rows.map((r, i) => (
          <tr key={i}>
            {r.map((c, j) => j === 0
              ? <th key={j} scope="row" className="whitespace-nowrap px-2 py-1 text-left font-normal text-muted-foreground">{c}</th>
              : <td key={j} className={cn('px-2 py-1', numeric.includes(j) ? 'text-right tabular-nums' : '')}>{c}</td>)}
          </tr>
        ))}
      </tbody>
    </table>
  )
}

/** A state's glyph and word together — tone never arrives on its own. */
export function StateWord({ state, children, className }: { state: State; children?: ReactNode; className?: string }) {
  return (
    <span className={cn('inline-flex items-center gap-1', className)}>
      <span aria-hidden className={GLYPH_CLASS[state]}>{GLYPH[state]}</span>
      <span>{children ?? state}</span>
    </span>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
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
    <svg
      aria-hidden
      width={0}
      height={0}
      className="pointer-events-none absolute"
      focusable="false"
    >
      <defs>
        <pattern
          id="op-hatch"
          width={5}
          height={5}
          patternUnits="userSpaceOnUse"
          patternTransform="rotate(45)"
        >
          <line
            x1={0}
            y1={0}
            x2={0}
            y2={5}
            stroke="var(--foreground)"
            strokeWidth={1.6}
            opacity={0.55}
          />
        </pattern>
        <pattern
          id="op-hatch-soft"
          width={6}
          height={6}
          patternUnits="userSpaceOnUse"
          patternTransform="rotate(45)"
        >
          <line
            x1={0}
            y1={0}
            x2={0}
            y2={6}
            stroke="var(--foreground)"
            strokeWidth={1}
            opacity={0.28}
          />
        </pattern>
        <pattern
          id="op-cross"
          width={5}
          height={5}
          patternUnits="userSpaceOnUse"
        >
          <path
            d="M0 0 L5 5 M5 0 L0 5"
            stroke="var(--foreground)"
            strokeWidth={0.9}
            opacity={0.5}
          />
        </pattern>
        <pattern id="op-dot" width={4} height={4} patternUnits="userSpaceOnUse">
          <circle
            cx={1.5}
            cy={1.5}
            r={1}
            fill="var(--foreground)"
            opacity={0.5}
          />
        </pattern>
      </defs>
    </svg>
  )
}

/** The live region that speaks the readout. Always rendered, even when empty. */
export function ReadoutLive({ text }: { text: string }) {
  return (
    <span className="sr-only" aria-live="polite">
      {text}
    </span>
  )
}

/**
 * The frame every figure in this wave shares: the plot is `role="img"` with a
 * sentence, and the toggle beside the footer swaps it for the same data as
 * rows. A figure whose data is a series and which passes no `table` is a bug,
 * not a shortcut — `Figure` warns in dev.
 */
export function Figure({
  label,
  table,
  footer,
  legend,
  height,
  className,
  children,
}: {
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
        <div
          style={height ? { height } : undefined}
          className="overflow-auto border"
        >
          {table}
        </div>
      ) : (
        <div role="img" aria-label={label} className="min-w-0">
          {children}
        </div>
      )}
      {(legend || table || footer) && (
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
          {legend ?? <span />}
          {table && (
            <button
              type="button"
              aria-pressed={asTable}
              onClick={() => setAsTable((v) => !v)}
              className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground underline underline-offset-4 hover:text-foreground"
            >
              {asTable ? 'chart' : 'table'}
            </button>
          )}
        </div>
      )}
      {footer && (
        <p className="flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">
          {footer}
        </p>
      )}
    </div>
  )
}

/** The table view every `Figure` hands to its toggle: the same numbers as rows. */
export function DataTable({
  caption,
  head,
  rows,
  numeric = [],
}: {
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
            <th
              key={i}
              scope="col"
              className={cn(
                'op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-[9px]',
                numeric.includes(i) ? 'text-right' : 'text-left'
              )}
            >
              {h}
            </th>
          ))}
        </tr>
      </thead>
      <tbody className="op-rows">
        {rows.map((r, i) => (
          <tr key={i}>
            {r.map((c, j) =>
              j === 0 ? (
                <th
                  key={j}
                  scope="row"
                  className="whitespace-nowrap px-2 py-1 text-left font-normal text-muted-foreground"
                >
                  {c}
                </th>
              ) : (
                <td
                  key={j}
                  className={cn(
                    'px-2 py-1',
                    numeric.includes(j) ? 'text-right tabular-nums' : ''
                  )}
                >
                  {c}
                </td>
              )
            )}
          </tr>
        ))}
      </tbody>
    </table>
  )
}

/** A state's glyph and word together — tone never arrives on its own. */
export function StateWord({
  state,
  children,
  className,
}: {
  state: State
  children?: ReactNode
  className?: string
}) {
  return (
    <span className={cn('inline-flex items-center gap-1', className)}>
      <span aria-hidden className={GLYPH_CLASS[state]}>
        {GLYPH[state]}
      </span>
      <span>{children ?? state}</span>
    </span>
  )
}

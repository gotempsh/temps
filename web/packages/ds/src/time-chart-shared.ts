// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { fmtNum } from './fmt'

/**
 * Every Temps time axis carries three things:
 *  1. deploy markers (dotted ink lines labelled with the deploy id), linked
 *     both ways to the deploy rows via `hot`/`onHot`
 *  2. the sampled window, if telemetry is being head-sampled
 *  3. the retention horizon of the plan, in the footer, with ranges past
 *     it still visible and explained (never hidden)
 * Lines are linear, ink on paper, no fills, no animation. The readout above
 * the plot shows the hovered or latest value so the chart works on touch.
 *
 * Deploys land in bursts. Markers whose labels would overlap (closer than
 * ~72px at the current width) collapse into one cluster label, "3 deploys",
 * while every deploy keeps its own dotted line. Clicking the cluster label
 * opens a strip under the plot listing its members (tag, time, note); hover
 * a member to light its line, click it to open the deploy (`onOpen`). The
 * axis never lies about how many deploys happened, and never overprints.
 *
 * Selecting time: drag across the plot to select a fraction of the axis. The
 * selection is an ink band with its bounds and point count in a strip under
 * the plot; `onSelect` receives `{ from, to }` (axis labels, inclusive) so the
 * page can narrow whatever sits under the chart (a ledger, a metric grid, the
 * status) to that window, and `null` when cleared (the strip's "clear" or
 * Escape). Pass `selection` to control it. A click without a drag clears.
 *
 * Series are told apart by pattern, never by hue: `stroke` ('solid' | 'dashed'
 * | 'dotted') and `weight` ('thin' | 'regular'), defaulted by position. The
 * legend is generated from `series` — the swatch is a sample of the real line
 * and carries the value at the cursor — so a hand-written key in the footer is
 * always wrong. Only a series that *is* a state (`series.state`) takes a tone.
 *
 * Every chart is readable without the picture: the plot is `role="img"` with a
 * sentence built from `title`, `range` and `verdict`, and the footer's "table"
 * toggle swaps the plot for the same buckets as rows, deploy markers included.
 */
export type TimeRange = { from: string; to: string }

export type TimePoint = { t: string } & Record<string, number | string>

export type Marker = { id: string; x: string; at?: string; note?: string }

/** How a line is drawn. Series are told apart by pattern, never by hue. */
export type SeriesStroke = 'solid' | 'dashed' | 'dotted'

export type SeriesWeight = 'thin' | 'regular'

export type Series = {
  key: string
  name: string
  /** Dash pattern. Defaults by position: solid, dashed, dotted, solid. */
  stroke?: SeriesStroke
  /** Line weight. Defaults: the first series `regular`, the rest `thin`. */
  weight?: SeriesWeight
  /** Exact pixel width. Overrides `weight`; kept for callers that tuned a line by hand. */
  width?: number
  /**
   * Only for a series that *is* a state — an error rate read against its
   * threshold. It takes that tone; every other series is ink. A series is
   * never coloured to tell it from a neighbour: that is what `stroke` is for.
   */
  state?: 'ok' | 'warn' | 'error'
  /**
   * Draw this line above the others regardless of its place in the legend. For
   * the one case where reading order and stacking order differ: an out-of-band
   * segment belongs last in the legend and on top of the line it marks.
   */
  top?: boolean
  /**
   * Off for a series that is a derived copy of another — the out-of-band
   * stretch of a line is the same numbers as the line, and the table already
   * has a `vs expected` column. A column of en dashes is not a fact.
   */
  inTable?: boolean
}

/** An expected range drawn behind the line: two keys of the same points, hatched. */
export type Band = { lower: string; upper: string; label?: string }

/** A point the model calls out of band. Drawn as a × on the line and listed by the caller. */
export type Anomaly = { x: string; note?: string; state?: 'warn' | 'error' }

/** The same measure over the period before this one, as a dotted ghost. */
export type Compare = { label: string; data: TimePoint[] }

/** Is this point outside its own expected range? */
export function outside(p: TimePoint, band: Band, key: string): 0 | 1 | -1 {
  const v = Number(p[key]),
    lo = Number(p[band.lower]),
    hi = Number(p[band.upper])
  if (!Number.isFinite(v) || !Number.isFinite(lo) || !Number.isFinite(hi))
    return 0
  if (v > hi) return 1
  if (v < lo) return -1
  return 0
}

/** "inside", "+141% above", "−22% below" — the column a reader scans for the excursions. */
export function vsExpected(p: TimePoint, band: Band, key: string): string {
  const side = outside(p, band, key)
  if (p[key] === undefined || p[band.lower] === undefined) return '—'
  if (!side) return 'inside'
  const v = Number(p[key])
  const bound = side > 0 ? Number(p[band.upper]) : Number(p[band.lower])
  const pct = bound ? ((v - bound) / Math.abs(bound)) * 100 : 0
  return `${pct >= 0 ? '+' : ''}${fmtNum(pct, { digits: Math.abs(pct) < 10 ? 1 : 0 })}% ${side > 0 ? 'above' : 'below'}`
}

/**
 * Range picker with the plan's retention horizon. Ranges beyond it are not
 * hidden: they render struck through and call `onGated` so the page can say
 * which plan keeps that range.
 */
export type Range = { label: string; days: number }

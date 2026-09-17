// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ReactNode } from 'react'
import { type State } from './status'

// ── Breakdown ──────────────────────────────────────────────────────────

/**
 * A ranked list of one dimension (country, browser, page, referrer): label,
 * count, share, and the share drawn as an ink bar behind the row. This is
 * what web draws ten times on the analytics overview. Rows with `children`
 * (a nested dimension: country → region → city, browser → version) get a
 * chevron and open in place; the header shows the path back.
 * `total` is the denominator for the share; when the top-N does not add up
 * to it, the remainder is one muted "other" row so the bars are honest.
 */
export type BreakdownRow = {
  label: ReactNode
  key?: string
  count: number
  state?: State
  /** What kind of thing the row is (a flag, a browser mark, a channel icon). Drawn in a fixed 16px slot so labels align. */ icon?: ReactNode
  children?: BreakdownRow[]
  onOpen?: () => void
}

// ── StatusStrip ────────────────────────────────────────────────────────

/**
 * Uptime over a window: one segment per bucket, coloured by its state, the
 * legend is the five glyphs. Hover or focus a segment to read it (start,
 * state, checks, p50/p95). The strip is the whole width of its cell so the
 * reader compares monitors by shape, not by number.
 */
export type StatusBucket = {
  start: string
  state: State
  checks?: number
  down?: number
  p50_ms?: number
  p95_ms?: number
}

// ── CalendarHeatmap ────────────────────────────────────────────────────

/**
 * Activity per day over weeks: a grid of 12px cells, columns are weeks, rows
 * are weekdays, five ink intensities. Ink, not green: the colour of a cell is
 * how much, not how well.
 *
 * The density is never the only encoding. The hovered or focused day reads in
 * full — date, count, and the deploy ids when `ids` is given — and the legend
 * prints the numbers behind the five swatches, derived from the data (for
 * example `0 · 1–2 · 3–4 · 5–6 · 7+`), so
 * a reader can tell a dark cell from a darker one without pointing at either.
 *
 * The readout follows the same rule as `GeoMap`: on a fine pointer it sits at
 * the cursor and nothing is added under the grid; below `md` (touch) it is a
 * row under the grid — tap a day to read it, tap it again to open it. The grid
 * itself is one focusable region: `←` `→` move a week, `↑` `↓` move a day, and
 * `⏎` opens the day when `onOpen` is set.
 */
export type ActivityDay = {
  date: string
  count: number
  /** What happened that day (deploy tags, run ids). Named in the readout instead of just counted. */ ids?: string[]
}

// ── Funnel ─────────────────────────────────────────────────────────────

/**
 * Steps of a funnel as bars whose width is the share of entrants still
 * present; under each, completions, conversion from the previous step, and
 * drop-off. Ink bars; the drop-off is the number that matters and is the
 * only thing that can turn red (above `dropAlert`).
 */
export type FunnelStep = { name: string; count: number; avgSeconds?: number }

// ── Flow ───────────────────────────────────────────────────────────────

/**
 * Transitions between pages: "from → to", count, share of the from-page's
 * exits. Not a Sankey: a ranked list is readable, sortable, and honest at
 * any width. Entry, exit and drop-off lists are the same rows with one
 * side empty.
 */
export type FlowRow = {
  from?: string
  to?: string
  count: number
  share: number
}

// ── Waterfall ──────────────────────────────────────────────────────────

/**
 * Spans of one trace: a tree on the left (collapsible), the bar on the
 * right placed by offset and width against the trace duration, the
 * duration in mono at the bar's end. Error spans get the × glyph and a red
 * bar; everything else is ink. Selecting a row is the caller's business
 * (`onSelect`), typically opening the span's attributes beside it.
 */
export type Span = {
  id: string
  name: string
  service?: string
  start_ms: number
  duration_ms: number
  state?: State
  children?: Span[]
}

// ── StackTrace ─────────────────────────────────────────────────────────

/**
 * Frames of one error, most recent first. In-app frames are ink and open
 * by default with their source context (line numbers in the gutter, the
 * failing line marked); vendor frames are muted and closed. A frame that
 * was symbolicated shows the original file in the gutter's corner. This is
 * the Sentry frame list without the card per frame.
 */
export type Frame = {
  fn: string
  file: string
  line: number
  col?: number
  inApp?: boolean
  original?: string
  context?: { line: number; code: string }[]
}

// ── LogLines · Stages ──────────────────────────────────────────────────

/**
 * Lines of a log: time in the gutter, level as a glyph (error ×, warn ◐,
 * everything else nothing), source in muted, the message in mono and
 * wrapping. A level filter is a row of toggles above; the count of hidden
 * lines is said. `live` pins the newest line at the bottom and says so.
 * Virtualisation is the console's job; this is the row.
 */
export type LogLine = {
  t: string
  level: 'error' | 'warn' | 'info' | 'debug'
  source?: string
  msg: string
}

/**
 * Stages of a build or run, in order, each with its state word and
 * duration; the running one is open and streams its `LogLines` beneath.
 * Finished stages open on click. One stage open at a time keeps the page
 * the length of its logs, not of every log.
 */
export type Stage = {
  name: string
  state: State
  duration?: string
  lines?: LogLine[]
  /** What the step produced, in its own units ("798 assets · 18.8 MB", "image 212 MB · 14 layers"). A step's line says its result, never its description: the reader knows what "build image" means, what they cannot know is what came out. On a failed step this is the failure in one clause. */
  result?: ReactNode
  /** Phase the step belongs to ("build", "release", "after going live"). A header is drawn where the phase changes; steps after going live do not hold the deploy back and read muted. */
  phase?: string
}

// ── Histogram · Percentiles ────────────────────────────────────────────

/**
 * A distribution and the statistic the reader picked from it: the
 * percentile selector is a Segmented (avg · p50 · p90 · p95 · p99), the
 * chosen one is a vertical rule through the bars with its value. Bars are
 * ink; the buckets past the selected percentile are muted so the tail is
 * visible. `buckets` are [upper_bound, count].
 */
export type HistBucket = { le: number; count: number }

export const PCTS = ['avg', 'p50', 'p90', 'p95', 'p99'] as const

export type Pct = (typeof PCTS)[number]

export function quantile(buckets: HistBucket[], q: number) {
  const total = buckets.reduce((a, b) => a + b.count, 0)
  let acc = 0,
    prevLe = 0
  for (const b of buckets) {
    if (acc + b.count >= q * total) {
      const need = q * total - acc
      return prevLe + (need / Math.max(1, b.count)) * (b.le - prevLe)
    }
    acc += b.count
    prevLe = b.le
  }
  return prevLe
}

// ── GeoMap: a choropleth by state, the second view of a "by country" list ──
export type GeoRow = {
  /** Name as it appears in the topojson (e.g. "United States of America"). */ geo: string
  label: string
  value: string
  state: State
  note?: string
}

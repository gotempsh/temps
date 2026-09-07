// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type ReactNode } from 'react'
import { cn } from './lib/cn'
import { fmtAbsolute, fmtDuration, fmtNum, fmtPct } from './fmt'
import { GLYPH, GLYPH_CLASS, type State } from './status'
import { TimeChart, vsExpected, type Band, type Marker, type Series, type TimePoint } from './time-chart'
import { DataTable, Figure, INK_FILL, INK_FILL_OPACITY, INK_FILL_WORD, INK_LAYER_ORDER, INK_TONE, ReadoutLive, inkCell, useReadout, type InkLayerFill } from './viz-ink'

/* ────────────────────────────────────────────────────────────────────────
   Forms with a time axis that `TimeChart` alone could not draw: an expected
   range, a composition, a latency grid, a resource's state, a recovery
   window, a session. All ink, all with a table view, all with a readout a
   keyboard can reach. See `design-system/docs/data-viz.md`.
   ──────────────────────────────────────────────────────────────────────── */

// ── BandChart ──────────────────────────────────────────────────────────

/** Which direction is the bad one for this metric. Decides what counts as an excursion. */
export type Worse = 'up' | 'down' | 'both'
/** One stretch of the line outside its expected range, derived — never hand-listed. */
export type Excursion = {
  /** Axis label of the peak of the excursion (the worst point, not the first). */
  x: string
  /** The measured value at the peak. */
  value: number
  /** The bound it broke, at the peak. */
  bound: number
  /** Signed distance from the bound, as a percentage of it. */
  pct: number
  /** Which way it went out. */
  side: 'above' | 'below'
  /** `warn` normally, `error` when the peak also crosses a threshold. */
  state: 'warn' | 'error'
  /** How many buckets the excursion lasted. */
  points: number
  /** The deploy whose marker is closest at or before the peak, if any. */
  deploy?: string
}

/** The reserved key the out-of-band stretch of the line is drawn from. */
const OUT = '__out'

/**
 * The metrics explorer's anomaly chart, read the way an operator reads one:
 *
 *  1. the expected range is a hatched ink band, bounds taken from the data
 *     (`band={{ lower, upper }}`), and the measured value is the ink line;
 *  2. where the line leaves the band, **that segment** is drawn in the state
 *     tone with a × at its peak. Colour is allowed there, and only there,
 *     because the segment *is* a state — a value outside its own model. Which
 *     way counts as out is `worse` (`up` for latency and errors, `down` for
 *     throughput and conversion, `both` for a ratio);
 *  3. nothing is flooded: no red area, no shaded rectangle behind the plot.
 *     A background wash cannot be compared with anything and hides the band;
 *  4. the footer states the excursions as facts with the deploy beside them —
 *     `3 anomalies · worst +42% at 14:20 ┆ dep_91a · band: rolling 7d` — and
 *     the legend is generated: expected · actual · anomaly;
 *  5. the table view carries a `vs expected` column (`inside`, `+141% above`);
 *  6. the `aria-label` states how many anomalies there were and the worst one;
 *  7. the readout at the cursor reads actual · expected range · delta.
 *
 * The excursions are derived from the data and the band, so the picture, the
 * footer, the list and the sentence cannot disagree about how many there were.
 * It stays a `TimeChart` with `band` and `anomalies`, not a second engine.
 *
 * ```tsx
 * <BandChart data={points} actual="p95" band={{ lower: 'lo', upper: 'hi' }}
 *   worse="up" errorAt={800} bandNote="rolling 7d, ±2σ"
 *   markers={[{ id: 'dep_91a', x: '10:00' }]}
 *   unit="ms" title="p95 · api-gateway" range="last 24h"
 *   verdict="Inside the band except one spike at 10:00." />
 * ```
 */
export function BandChart({ data, actual = 'value', actualName = 'actual', band, worse = 'up', errorAt, bandNote, markers = [], unit, title, range, verdict, height = 200, xInterval, footer, onOpen, className }: {
  data: TimePoint[]
  /** Key of the measured series. Drawn solid and regular, over the band. */
  actual?: string
  /** Name of the measured series in the generated legend. */
  actualName?: string
  /** The two keys that bound the expected range, and what to call it. */
  band: Band
  /** Which side of the band is the bad one. Latency and errors are `up`; throughput and conversion are `down`. */
  worse?: Worse
  /** A hard limit that turns an excursion from `warn` into `error` (the budget, the SLO). */
  errorAt?: number
  /** How the band was computed ("rolling 7d, ±2σ"). Stated in the footer: a band nobody can explain is a decoration. */
  bandNote?: string
  /** Deploy markers. The footer names the deploy nearest each excursion. */
  markers?: Marker[]
  unit?: string
  title: string
  range: string
  verdict: string
  height?: number
  xInterval?: number
  /** Extra `ChartFooter` facts (bucket, retention). The anomaly summary is added for you. */
  footer?: ReactNode
  /** Open the anomaly — the metric at that second, the alert it fired. */
  onOpen?: (e: Excursion) => void
  className?: string
}) {
  const u = unit ? ` ${unit}` : ''
  // Every excursion is derived here, once: a contiguous run of points outside
  // the band, reported at its worst point rather than its first.
  const { plot, excursions } = useMemo(() => {
    const side = (p: TimePoint): 'above' | 'below' | null => {
      const v = Number(p[actual]), lo = Number(p[band.lower]), hi = Number(p[band.upper])
      if (!Number.isFinite(v) || !Number.isFinite(lo) || !Number.isFinite(hi)) return null
      if (v > hi && worse !== 'down') return 'above'
      if (v < lo && worse !== 'up') return 'below'
      return null
    }
    const out: Excursion[] = []
    const flags = data.map(side)
    let i = 0
    while (i < data.length) {
      if (!flags[i]) { i += 1; continue }
      let j = i
      while (j + 1 < data.length && flags[j + 1] === flags[i]) j += 1
      const dir = flags[i] as 'above' | 'below'
      let peak = i
      for (let k = i; k <= j; k += 1) {
        const better = dir === 'above' ? Number(data[k][actual]) > Number(data[peak][actual]) : Number(data[k][actual]) < Number(data[peak][actual])
        if (better) peak = k
      }
      const value = Number(data[peak][actual])
      const bound = dir === 'above' ? Number(data[peak][band.upper]) : Number(data[peak][band.lower])
      const crossed = errorAt !== undefined && (worse === 'down' ? value <= errorAt : value >= errorAt)
      const before = [...markers].filter((m) => data.findIndex((p) => p.t === m.x) <= peak && data.findIndex((p) => p.t === m.x) >= 0).pop()
      out.push({
        x: String(data[peak].t), value, bound,
        pct: bound ? ((value - bound) / Math.abs(bound)) * 100 : 0,
        side: dir, state: crossed ? 'error' : 'warn', points: j - i + 1, deploy: before?.id,
      })
      i = j + 1
    }
    // The out-of-band stretch is its own series so the tone lands on the
    // segment and nowhere else; the crossing points on either side are
    // included so a one-bucket excursion is still a line and not a gap.
    const inRun = data.map((_, n) => {
      if (flags[n]) return true
      return Boolean(flags[n - 1]) || Boolean(flags[n + 1])
    })
    const plot: TimePoint[] = data.map((p, n) => (inRun[n] ? { ...p, [OUT]: Number(p[actual]) } : p))
    return { plot, excursions: out }
  }, [data, actual, band.lower, band.upper, worse, errorAt, markers])

  const worst = excursions.reduce<Excursion | null>((b, e) => (b === null || Math.abs(e.pct) > Math.abs(b.pct) ? e : b), null)
  const tone = excursions.some((e) => e.state === 'error') ? 'error' : 'warn'
  const at = useMemo(() => new Map(data.map((p) => [p.t, p])), [data])
  const summary = excursions.length === 0
    ? 'no anomalies'
    : `${excursions.length} anomal${excursions.length === 1 ? 'y' : 'ies'} · worst ${worst && worst.pct >= 0 ? '+' : ''}${fmtNum(worst?.pct ?? 0, { digits: Math.abs(worst?.pct ?? 0) < 10 ? 1 : 0 })}% at ${worst?.x}${worst?.deploy ? ` ┆ ${worst.deploy}` : ''}`
  // The anomaly reads last in the legend and draws on top of the line it marks.
  const series: Series[] = excursions.length
    ? [{ key: actual, name: actualName }, { key: OUT, name: 'anomaly', state: tone, stroke: 'solid', weight: 'regular', top: true, inTable: false }]
    : [{ key: actual, name: actualName }]
  return (
    <div className={cn('min-w-0 space-y-2', className)}>
      <TimeChart
        data={plot} series={series} band={band}
        anomalies={excursions.map((e) => ({ x: e.x, note: e.side, state: e.state }))}
        markers={markers} unit={unit} height={height} xInterval={xInterval}
        title={title} range={range}
        verdict={`${verdict.replace(/\.\s*$/, '')}. ${summary}`}
        // actual · expected range · delta, in that order, at the cursor.
        readoutFormat={(p) => {
          const lo = p[band.lower], hi = p[band.upper]
          const v = fmtNum(Number(p[actual]))
          const rangeWord = lo === undefined ? '' : ` · expected ${fmtNum(Number(lo))}–${fmtNum(Number(hi))}${u}`
          return `${p.t} · ${v}${u}${rangeWord} · ${vsExpected(p, band, actual)}`
        }} />
      {/* The footer states the anomalies as facts, and says what the band is:
          a band nobody can explain is a decoration. */}
      <p className="flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">
        {footer}
        <span className={excursions.length ? (tone === 'error' ? 'text-destructive' : 'text-warning') : undefined}>· {summary}</span>
        {bandNote && <span>· band: {bandNote}</span>}
      </p>
      {excursions.length > 0 && (
        <ol className="op-rows border bg-background font-mono text-[11px]">
          <li className="op-label px-2 py-1 text-[9px]">{excursions.length} outside the expected range</li>
          {excursions.map((e) => {
            const p = at.get(e.x)
            return (
              <li key={e.x}>
                <button type="button" disabled={!onOpen} onClick={() => onOpen?.(e)} className={cn('flex w-full flex-wrap items-baseline gap-x-3 px-2 py-1 text-left', onOpen && 'hover:bg-muted')}>
                  <span aria-hidden className={GLYPH_CLASS[e.state]}>×</span>
                  <span className="w-12 shrink-0">{e.x}</span>
                  <span className="tabular-nums">{fmtNum(e.value)}{unit ? <span className="text-muted-foreground">{unit}</span> : null}</span>
                  <span className={e.state === 'error' ? 'text-destructive' : 'text-warning'}>{e.pct >= 0 ? '+' : ''}{fmtNum(e.pct, { digits: Math.abs(e.pct) < 10 ? 1 : 0 })}% {e.side}</span>
                  <span className="text-muted-foreground">
                    expected {p ? `${fmtNum(Number(p[band.lower]))}–${fmtNum(Number(p[band.upper]))}${u}` : '—'}
                    {' · '}{e.points} bucket{e.points === 1 ? '' : 's'}
                    {e.deploy ? ` · ┆ ${e.deploy}` : ''}
                  </span>
                  {onOpen && <span className="ml-auto shrink-0 text-muted-foreground">open ↗</span>}
                </button>
              </li>
            )
          })}
        </ol>
      )}
    </div>
  )
}

// ── StackedInk ─────────────────────────────────────────────────────────

/** One layer of a composition. Four is the ceiling; a fifth is a table. */
export type InkLayer = {
  key: string
  name: string
  /** Pattern fill. Defaults by position: solid, hatched, dotted, cross-hatched. */
  fill?: InkLayerFill
  /**
   * Only for the layer that *is* a state — 5xx in a status split, `error` in a
   * log-level split. It takes that tone; every other layer is ink.
   */
  state?: 'ok' | 'warn' | 'error'
}

/**
 * Composition over time, as stacked **bars** from a zero baseline — never a
 * stacked area, which is banned: an area's middle bands have no baseline and
 * cannot be compared. Layers are told apart by pattern at three greys, so the
 * figure survives a monochrome print and a colour-blind reader.
 *
 * Use it for status classes (2xx/3xx/4xx/5xx), tokens by model, log volume by
 * level, backup size by source. When the reader wants the share of the whole
 * range rather than its shape over time, that is `Breakdown`, not this.
 *
 * ```tsx
 * <StackedInk data={T} layers={[{ key: 'ok', name: '2xx' }, { key: 'e4', name: '4xx' },
 *   { key: 'e5', name: '5xx', state: 'error' }]}
 *   unit="requests" title="requests by status class" range="last 1h"
 *   verdict="All 2xx except 91 × 502 at 10:44." />
 * ```
 */
export function StackedInk({ data, layers, unit = '', title, range, verdict, height = 160, xInterval, partial, footer, className }: {
  data: TimePoint[]
  layers: InkLayer[]
  unit?: string
  title: string
  range: string
  verdict: string
  height?: number
  /** Label every nth bucket on the x axis. Defaults to four labels. */
  xInterval?: number
  /** The last bucket is still filling: it is hatched over and the footer says so. */
  partial?: boolean
  footer?: ReactNode
  className?: string
}) {
  const { i, setI, regionProps } = useReadout(data.length)
  if (import.meta.env.DEV && layers.length > 4) console.warn(`[chart] StackedInk has ${layers.length} layers; four patterns is the limit of what the eye separates. Use a table (data-viz.md §StackedInk).`)
  const totals = data.map((p) => layers.reduce((a, l) => a + (Number(p[l.key]) || 0), 0))
  const max = Math.max(1, ...totals)
  const grand = totals.reduce((a, b) => a + b, 0)
  const sum = (k: string) => data.reduce((a, p) => a + (Number(p[k]) || 0), 0)
  const step = xInterval ?? Math.max(1, Math.floor(data.length / 4))
  const read = i === null ? null : data[i]
  const readText = read ? `${read.t} · ${layers.map((l) => `${l.name} ${fmtNum(Number(read[l.key]) || 0)}`).join(' · ')}` : ''
  const fillOf = (l: InkLayer, n: number) => (l.state ? INK_TONE[l.state] : INK_FILL[l.fill ?? INK_LAYER_ORDER[n % 4]])
  const opacityOf = (l: InkLayer, n: number) => (l.state ? 0.9 : INK_FILL_OPACITY[l.fill ?? INK_LAYER_ORDER[n % 4]])
  const label = `${title}${unit ? ` in ${unit}` : ''}, ${range}, ${layers.length} layers stacked from zero. ${verdict.replace(/\.\s*$/, '')}. ${data.length} buckets; switch to the table view to read every value.`
  return (
    <Figure
      className={className}
      label={label}
      footer={<>{footer}{partial && <span>· ▨ the current bucket is still filling</span>}</>}
      legend={
        <ul className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
          {layers.map((l, n) => (
            <li key={l.key} className="flex items-center gap-1.5">
              <svg aria-hidden width={14} height={10} viewBox="0 0 14 10" className="shrink-0"><rect width={14} height={10} fill={fillOf(l, n)} fillOpacity={opacityOf(l, n)} stroke="var(--op-rule-soft)" /></svg>
              {l.state && <span aria-hidden className={GLYPH_CLASS[l.state]}>{GLYPH[l.state]}</span>}
              <span>{l.name}</span>
              <span className="tabular-nums text-foreground">{read ? fmtNum(Number(read[l.key]) || 0) : fmtNum(sum(l.key))}</span>
              <span>{read ? '' : `· ${fmtPct(grand ? sum(l.key) / grand : 0, { basis: 'ratio', digits: 1 })}`}</span>
              {/* The pattern is named, so the legend works read aloud too. */}
              <span className="sr-only">{l.fill ? INK_FILL_WORD[l.fill] : INK_FILL_WORD[INK_LAYER_ORDER[n % 4]]}</span>
            </li>
          ))}
        </ul>
      }
      table={<DataTable caption={label} head={['bucket', ...layers.map((l) => `${l.name}${unit ? ` (${unit})` : ''}`), 'total']} numeric={layers.map((_, n) => n + 1).concat([layers.length + 1])}
        rows={data.map((p, n) => [p.t, ...layers.map((l) => fmtNum(Number(p[l.key]) || 0)), fmtNum(totals[n])])} />}
    >
      <div className="min-w-0">
        <div className="flex items-baseline justify-between font-mono text-[11px]">
          <span className="tabular-nums">{read ? readText : `${range} · ${fmtNum(grand)}${unit ? ` ${unit}` : ''}`}</span>
          <span className="text-muted-foreground">{read ? 'bucket' : 'total'}</span>
        </div>
        {/* One focusable region; ← → walk the buckets and announce each. */}
        <div {...regionProps} aria-label={`${data.length} buckets · use arrow keys to read each`} className={cn(regionProps.className, 'mt-1')}>
          <svg viewBox={`0 0 ${data.length * 10} 100`} preserveAspectRatio="none" style={{ height }} className="block w-full" aria-hidden>
            {data.map((p, n) => {
              let acc = 0
              return (
                <g key={p.t} onMouseEnter={() => setI(n)} onMouseLeave={() => setI(null)}>
                  <rect x={n * 10} y={0} width={10} height={100} fill="transparent" />
                  {layers.map((l, li) => {
                    const v = Number(p[l.key]) || 0
                    const h = (v / max) * 100
                    const y = 100 - acc - h
                    acc += h
                    return h > 0 ? <rect key={l.key} x={n * 10 + 0.6} y={y} width={8.8} height={h} fill={fillOf(l, li)} fillOpacity={opacityOf(l, li)} /> : null
                  })}
                  {partial && n === data.length - 1 && <rect x={n * 10 + 0.6} y={100 - acc} width={8.8} height={acc} fill="url(#op-hatch)" />}
                  {i === n && <rect x={n * 10 + 0.6} y={0} width={8.8} height={100} fill="none" stroke="var(--foreground)" strokeWidth={1} vectorEffect="non-scaling-stroke" />}
                </g>
              )
            })}
          </svg>
          <div className="mt-1 flex justify-between font-mono text-[10px] text-muted-foreground">
            {data.filter((_, n) => n % step === 0).map((p) => <span key={p.t}>{p.t}</span>)}
          </div>
        </div>
        <ReadoutLive text={readText} />
      </div>
    </Figure>
  )
}

// ── LatencyHeatmap ─────────────────────────────────────────────────────

/**
 * Time × latency bucket, ink density by count. The chart that answers "is the
 * p95 one slow route or all of them?" when a `Histogram` collapses the time
 * axis and a percentile line hides the second mode: two horizontal bands in
 * this grid *are* two populations.
 *
 * Rows are latency buckets, newest bucket last on x. Density is the same five
 * ink steps as `CalendarHeatmap`; a cell with no samples is not a light
 * something, it is the empty step. Arrow keys walk the grid and announce the
 * cell; the table view has the same numbers.
 *
 * ```tsx
 * <LatencyHeatmap columns={hours} rows={[{ le: 50 }, { le: 100 }, { le: 250 }]}
 *   counts={grid} unit="ms" title="request latency" range="last 24h"
 *   verdict="A second band at 250-500ms appears after 10:00."
 *   overlays={[{ name: 'p95', values: p95 }]} />
 * ```
 */
export type LatencyRow = { le: number; label?: string }
export function LatencyHeatmap({ columns, rows, counts, unit = 'ms', title, range, verdict, overlays = [], cell = 18, footer, className }: {
  /** Time buckets, left to right. */
  columns: string[]
  /** Latency buckets, fastest first. `le` is the inclusive upper bound. */
  rows: LatencyRow[]
  /** `counts[row][column]` — how many requests landed in that bucket. */
  counts: number[][]
  unit?: string
  title: string
  range: string
  verdict: string
  /** p50 / p95 / p99 drawn over the grid, in the same latency units. Optional. */
  overlays?: { name: string; values: number[]; stroke?: 'solid' | 'dashed' | 'dotted' }[]
  /** Row height in px. */
  cell?: number
  footer?: ReactNode
  className?: string
}) {
  const [at, setAt] = useState<{ r: number; c: number } | null>(null)
  const max = Math.max(1, ...counts.flat())
  const total = counts.flat().reduce((a, b) => a + b, 0)
  const rowLabel = (r: LatencyRow, n: number) => r.label ?? `${n === 0 ? '0' : fmtNum(rows[n - 1].le)}–${fmtNum(r.le)}`
  // Rows are drawn fastest first, and the buckets are not linear (25, 50, 100,
  // 250 …), so an overlay is placed by *which row it falls in* and where inside
  // that row — not by a linear scale, which would draw p95 in the wrong band.
  const yOf = (v: number) => {
    let r = rows.findIndex((b) => v <= b.le)
    if (r < 0) r = rows.length - 1
    const lower = r === 0 ? 0 : rows[r - 1].le
    const frac = Math.max(0, Math.min(1, (v - lower) / Math.max(1e-9, rows[r].le - lower)))
    return ((r + frac) / rows.length) * 100
  }
  const read = at ? `${columns[at.c]} · ${rowLabel(rows[at.r], at.r)}${unit} · ${fmtNum(counts[at.r][at.c])} requests` : ''
  const label = `${title}, ${range}, ${rows.length} latency buckets by ${columns.length} time buckets, ink density is the count. ${verdict.replace(/\.\s*$/, '')}. ${fmtNum(total)} requests; switch to the table view to read every cell.`
  const move = (dr: number, dc: number) => setAt((p) => {
    const n = p ?? { r: rows.length - 1, c: 0 }
    return { r: Math.max(0, Math.min(rows.length - 1, n.r + dr)), c: Math.max(0, Math.min(columns.length - 1, n.c + dc)) }
  })
  const DASH = { solid: undefined, dashed: '4 3', dotted: '1 3' } as const
  return (
    <Figure
      className={className}
      label={label}
      footer={footer}
      legend={
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
          <span className="flex items-center gap-1">none {[0, 1, 2, 3, 4].map((s) => <span key={s} className="block h-2 w-2" style={{ backgroundColor: 'var(--foreground)', opacity: [0.06, 0.22, 0.42, 0.68, 1][s] }} />)} {fmtNum(max)}</span>
          {overlays.map((o) => (
            <span key={o.name} className="flex items-center gap-1.5">
              <svg aria-hidden width={18} height={6} viewBox="0 0 18 6" className="shrink-0"><line x1={0} y1={3} x2={18} y2={3} stroke="var(--foreground)" strokeWidth={1} strokeDasharray={DASH[o.stroke ?? 'dashed']} /></svg>{o.name}
            </span>
          ))}
        </div>
      }
      table={<DataTable caption={label} head={[`latency (${unit})`, ...columns]} numeric={columns.map((_, n) => n + 1)}
        rows={rows.map((r, n) => [rowLabel(r, n), ...counts[n].map((c) => fmtNum(c))])} />}
    >
      <div className="min-w-0">
        <div className="flex items-baseline justify-between font-mono text-[11px]">
          <span className="tabular-nums">{read || `${fmtNum(total)} requests · ${range}`}</span>
          <span className="text-muted-foreground">{at ? 'cell' : 'total'}</span>
        </div>
        <div className="mt-1 flex min-w-0 gap-2">
          <ol className="shrink-0 font-mono text-[10px] text-muted-foreground">
            {rows.map((r, n) => <li key={r.le} className="flex items-center justify-end" style={{ height: cell }}>{rowLabel(r, n)}</li>)}
          </ol>
          <div
            role="group" tabIndex={0} aria-label={`${rows.length} by ${columns.length} grid · use arrow keys to read a cell`}
            onFocus={() => setAt((p) => p ?? { r: rows.length - 1, c: 0 })} onBlur={() => setAt(null)}
            onKeyDown={(e) => {
              const k = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[e.key]
              if (k) { e.preventDefault(); move(k[0], k[1]) }
            }}
            className="min-w-0 flex-1 outline-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring"
          >
            {/* The overlay is positioned over the cells only, never over the axis
                labels, and lands in the row its value belongs to. */}
            <div className="relative min-w-0">
              {rows.map((_, r) => (
                <div key={r} className="flex gap-px" style={{ height: cell }}>
                  {counts[r].map((v, c) => (
                    <span key={c} aria-hidden onMouseEnter={() => setAt({ r, c })} onMouseLeave={() => setAt(null)}
                      className={cn('min-w-0 flex-1', at && at.r === r && at.c === c && 'ring-1 ring-foreground')}
                      style={inkCell(v, max)} />
                  ))}
                </div>
              ))}
              {overlays.length > 0 && (
                <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="pointer-events-none absolute inset-0 h-full w-full">
                  {overlays.map((o) => (
                    <polyline key={o.name} fill="none" stroke="var(--foreground)" strokeWidth={1.25} vectorEffect="non-scaling-stroke" strokeDasharray={DASH[o.stroke ?? 'dashed']}
                      points={o.values.map((v, n) => `${((n + 0.5) / columns.length) * 100},${yOf(v)}`).join(' ')} />
                  ))}
                </svg>
              )}
            </div>
            <div className="mt-1 flex justify-between font-mono text-[10px] text-muted-foreground">
              {columns.filter((_, n) => n % Math.max(1, Math.floor(columns.length / 4)) === 0).map((c) => <span key={c}>{c}</span>)}
            </div>
          </div>
        </div>
        <ReadoutLive text={read} />
      </div>
    </Figure>
  )
}

// ── StateTimeline ──────────────────────────────────────────────────────

/** One stretch of time a resource spent in one state. */
export type StateSegment = {
  /** The state's tone; the word beside it is `word`. */
  state: State
  /** What the state is called here ("up", "degraded", "building", "no heartbeat"). */
  word: string
  /** When the segment started, as the label the axis prints. */
  from: string
  /** How long it lasted, in seconds. The segment's width *is* this number. */
  seconds: number
  /** One clause about why, shown in the readout and the table. */
  note?: string
}

/**
 * A resource's state over time as one horizontal bar of segments, each as wide
 * as it was long: uptime up/degraded/down, a deployment's lifecycle, a node's
 * heartbeat. The durations are the point — three one-minute blips and one
 * ninety-minute outage are not the same incident, and a bucketed strip draws
 * them the same.
 *
 * **`StateTimeline` or `StatusStrip`?** `StatusStrip` buckets time into equal
 * segments, so a row of monitors can be compared by shape in a ledger; it
 * cannot say how long anything lasted. `StateTimeline` draws the real
 * transitions with their durations, so it belongs on the record of one
 * resource. Never draw both for the same window on the same screen.
 *
 * ```tsx
 * <StateTimeline segments={segs} title="acme.sh checks" range="last 24h"
 *   verdict="Up all day except 30 minutes down at 20:30." />
 * ```
 */
export function StateTimeline({ segments, title, range, verdict, height = 22, footer, onOpen, className }: {
  segments: StateSegment[]
  title: string
  range: string
  verdict: string
  height?: number
  footer?: ReactNode
  /** Open the incident, the deploy, the heartbeat gap. */
  onOpen?: (s: StateSegment, index: number) => void
  className?: string
}) {
  const { i, setI, regionProps } = useReadout(segments.length)
  const total = segments.reduce((a, s) => a + s.seconds, 0) || 1
  const TONE: Record<State, string> = { ok: 'bg-success', warn: 'bg-warning', error: 'bg-destructive', idle: 'bg-muted-foreground/40', sampled: 'bg-foreground/20', running: 'bg-foreground' }
  const seg = i === null ? null : segments[i]
  const readText = seg ? `${seg.from} · ${seg.word} · ${fmtDuration(seg.seconds * 1000)}${seg.note ? ` · ${seg.note}` : ''}` : ''
  // The legend is generated from the states actually present, never typed.
  const words = segments.reduce<{ state: State; word: string; seconds: number }[]>((a, s) => {
    const hit = a.find((x) => x.word === s.word)
    if (hit) hit.seconds += s.seconds
    else a.push({ state: s.state, word: s.word, seconds: s.seconds })
    return a
  }, [])
  const label = `${title}, ${range}, ${segments.length} state changes. ${verdict.replace(/\.\s*$/, '')}. ${words.map((w) => `${w.word} ${fmtPct(w.seconds / total, { basis: 'ratio', digits: 2 })}`).join(', ')}; switch to the table view to read every segment.`
  return (
    <Figure
      className={className}
      label={label}
      footer={footer}
      legend={
        <ul className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
          {words.map((w) => (
            <li key={w.word} className="flex items-center gap-1.5">
              <span aria-hidden className={GLYPH_CLASS[w.state]}>{GLYPH[w.state]}</span>{w.word}
              <span className="tabular-nums text-foreground">{fmtPct(w.seconds / total, { basis: 'ratio', digits: 2 })}</span>
              <span>· {fmtDuration(w.seconds * 1000)}</span>
            </li>
          ))}
        </ul>
      }
      table={<DataTable caption={label} head={['from', 'state', 'for', 'share', 'why']} numeric={[2, 3]}
        rows={segments.map((s) => [s.from, <span key="s" className={GLYPH_CLASS[s.state]}>{GLYPH[s.state]} {s.word}</span>, fmtDuration(s.seconds * 1000), fmtPct(s.seconds / total, { basis: 'ratio', digits: 2 }), s.note ?? '—'])} />}
    >
      <div className="min-w-0">
        <div className="flex items-baseline justify-between font-mono text-[11px]">
          <span className="tabular-nums">{readText || `${range} · ${segments.length} state changes`}</span>
          <span className="text-muted-foreground">{seg ? 'segment' : 'window'}</span>
        </div>
        <div {...regionProps} aria-label={`${segments.length} segments · use arrow keys to read each`} className={cn(regionProps.className, 'mt-1 flex gap-px')} style={{ height }}>
          {segments.map((s, n) => (
            <span key={n} aria-hidden onMouseEnter={() => setI(n)} onMouseLeave={() => setI(null)} onClick={() => onOpen?.(s, n)}
              className={cn('min-w-[2px]', TONE[s.state], i === n && 'ring-1 ring-foreground', onOpen && 'cursor-pointer')}
              style={{ width: `${(s.seconds / total) * 100}%` }} />
          ))}
        </div>
        <div className="mt-1 flex justify-between font-mono text-[10px] text-muted-foreground">
          <span>{segments[0]?.from}</span><span>{range}</span><span>now</span>
        </div>
        <ReadoutLive text={readText} />
      </div>
    </Figure>
  )
}

// ── WindowTimeline ─────────────────────────────────────────────────────

/**
 * What a restore can actually reach: full backups as marks, the window the
 * write-ahead log covers as a hatched band, and the restore target as a cursor
 * on the same axis. It sits directly above the point-in-time field so the
 * operator sees why a second is refused before they type it.
 *
 * Times are ISO local stamps (`2026-09-06T18:33:00`), the same strings
 * `DateTimeField` reads and writes, so the cursor and the field cannot drift.
 *
 * ```tsx
 * <WindowTimeline from={FLOOR} to={CEIL} covered={[{ from: FLOOR, to: CEIL, label: 'WAL' }]}
 *   marks={[{ at: '2026-09-06T02:00:00', label: 'b_41' }]} cursor={{ at: pit, label: 'restore to' }}
 *   title="recoverable window" verdict="Any second in the last 7 days." />
 * ```
 */
export type WindowMark = { at: string; label: string; state?: State; note?: string }
export function WindowTimeline({ from, to, covered = [], marks = [], cursor, title, verdict, zone, height = 34, footer, className }: {
  /** Start of the axis, ISO local. */
  from: string
  /** End of the axis, ISO local. */
  to: string
  /** Stretches the log covers. Hatched: this is a range, not a point. */
  covered?: { from: string; to: string; label: string }[]
  /** Full backups, upgrades, the moment a schedule failed. */
  marks?: WindowMark[]
  /** Where a restore would land. */
  cursor?: { at: string; label: string }
  title: string
  verdict: string
  /** The clock the stamps are read in. A time with no zone beside it is a guess. */
  zone?: string
  height?: number
  footer?: ReactNode
  className?: string
}) {
  const t0 = Date.parse(from), t1 = Date.parse(to)
  const span = Math.max(1, t1 - t0)
  const pos = (iso: string) => Math.max(0, Math.min(100, ((Date.parse(iso) - t0) / span) * 100))
  const { i, setI, regionProps } = useReadout(marks.length)
  const mark = i === null ? null : marks[i]
  const readText = mark ? `${mark.label} · ${fmtAbsolute(mark.at, { seconds: true })}${zone ? ` ${zone}` : ''}${mark.note ? ` · ${mark.note}` : ''}` : ''
  const outside = cursor && (Date.parse(cursor.at) < t0 || Date.parse(cursor.at) > t1)
  const label = `${title}, ${fmtAbsolute(from)} to ${fmtAbsolute(to)}, ${marks.length} backups and ${covered.length} covered window${covered.length === 1 ? '' : 's'}. ${verdict.replace(/\.\s*$/, '')}.${cursor ? ` The restore target is ${fmtAbsolute(cursor.at, { seconds: true })}${outside ? ', outside the window' : ''}.` : ''} Switch to the table view to read every backup.`
  return (
    <Figure
      className={className}
      label={label}
      footer={footer}
      legend={
        <ul className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
          <li className="flex items-center gap-1.5"><svg aria-hidden width={14} height={8} viewBox="0 0 14 8"><rect width={14} height={8} fill="url(#op-hatch-soft)" stroke="var(--op-rule-soft)" /></svg>{covered[0]?.label ?? 'covered'}</li>
          <li className="flex items-center gap-1.5"><span aria-hidden>▎</span>full backup</li>
          {cursor && <li className="flex items-center gap-1.5"><span aria-hidden className={outside ? 'text-destructive' : 'text-foreground'}>▼</span>{cursor.label} <span className="tabular-nums text-foreground">{fmtAbsolute(cursor.at, { seconds: true })}{zone ? ` ${zone}` : ''}</span></li>}
        </ul>
      }
      table={<DataTable caption={label} head={['backup', 'taken', 'state', 'note']}
        rows={marks.map((m) => [m.label, `${fmtAbsolute(m.at, { seconds: true })}${zone ? ` ${zone}` : ''}`, m.state ? <span key="s" className={GLYPH_CLASS[m.state]}>{GLYPH[m.state]} {m.state}</span> : 'ok', m.note ?? '—'])} />}
    >
      <div className="min-w-0">
        <div className="flex items-baseline justify-between font-mono text-[11px]">
          <span className="tabular-nums">{readText || (cursor ? `${cursor.label} ${fmtAbsolute(cursor.at, { seconds: true })}${zone ? ` ${zone}` : ''}` : title)}</span>
          <span className="text-muted-foreground">{mark ? 'backup' : cursor ? 'target' : ''}</span>
        </div>
        <div {...regionProps} aria-label={`${marks.length} backups · use arrow keys to read each`} className={cn(regionProps.className, 'relative mt-1 border')} style={{ height }}>
          {covered.map((c) => (
            <span key={c.from} aria-hidden className="op-ink-hatch absolute inset-y-0" style={{ left: `${pos(c.from)}%`, width: `${pos(c.to) - pos(c.from)}%` }} />
          ))}
          {marks.map((m, n) => (
            <span key={m.at} aria-hidden onMouseEnter={() => setI(n)} onMouseLeave={() => setI(null)}
              className={cn('absolute inset-y-0 w-px', m.state === 'error' ? 'bg-destructive' : 'bg-foreground', i === n && 'w-0.5')}
              style={{ left: `${pos(m.at)}%` }} />
          ))}
          {cursor && !outside && (
            <span aria-hidden className="absolute inset-y-0 w-px bg-foreground" style={{ left: `${pos(cursor.at)}%` }}>
              <span className="absolute -top-px left-1/2 -translate-x-1/2 font-mono text-[9px] leading-none">▼</span>
            </span>
          )}
        </div>
        <div className="mt-1 flex justify-between font-mono text-[10px] text-muted-foreground">
          <span>{fmtAbsolute(from)}</span>{zone && <span>{zone}</span>}<span>{fmtAbsolute(to)}</span>
        </div>
        <ReadoutLive text={readText} />
      </div>
    </Figure>
  )
}

// ── SessionTimeline ────────────────────────────────────────────────────

/** What happened in a session, at a millisecond offset from its start. */
export type SessionEvent = {
  at_ms: number
  kind: 'pageview' | 'click' | 'input' | 'network' | 'error' | 'custom'
  label: string
  /** Only for an event that is a state: a failed request, a thrown error. */
  state?: State
  note?: string
}
const KIND_GLYPH: Record<SessionEvent['kind'], string> = { pageview: '▤', click: '◆', input: '▮', network: '↔', error: '×', custom: '◇' }
/** An offset into a session. Zero is the start of the recording, not "0µs". */
const offset = (ms: number) => (ms === 0 ? '0s' : fmtDuration(ms))

/**
 * The timeline half of session replay: the session's events on a time axis
 * with the scrubber's position, and the same events as a synchronised list.
 * The player itself (rrweb) is not ours; this is the contract around it —
 * `position_ms` in, `onSeek` out.
 *
 * The list is the primary view and carries the keyboard: `←` `→` step events,
 * Enter seeks. The axis is the second view of the same rows, the way `GeoMap`
 * is the second view of a ranked list. Errors are the only events with a tone.
 *
 * ```tsx
 * <SessionTimeline duration_ms={214_000} events={events}
 *   position_ms={pos} onSeek={setPos} title="session ses_8c1" />
 * ```
 */
export function SessionTimeline({ duration_ms, events, position_ms = 0, onSeek, title, verdict, footer, className }: {
  duration_ms: number
  events: SessionEvent[]
  /** Where the player is. Drawn as the cursor and used to mark the current row. */
  position_ms?: number
  /** Seeking the player. Without it the timeline is read-only and says so. */
  onSeek?: (ms: number) => void
  title: string
  verdict: string
  footer?: ReactNode
  className?: string
}) {
  const { i, setI, regionProps } = useReadout(events.length)
  const pos = (ms: number) => Math.max(0, Math.min(100, (ms / Math.max(1, duration_ms)) * 100))
  const current = events.reduce((best, e, n) => (e.at_ms <= position_ms ? n : best), -1)
  const sel = i === null ? null : events[i]
  const readText = sel ? `${offset(sel.at_ms)} · ${sel.kind} · ${sel.label}${sel.note ? ` · ${sel.note}` : ''}` : ''
  const kinds = [...new Set(events.map((e) => e.kind))]
  const label = `${title}, ${fmtDuration(duration_ms)} long, ${events.length} events. ${verdict.replace(/\.\s*$/, '')}. The list below the axis has every event with its offset.`
  return (
    <div className={cn('min-w-0 space-y-2', className)}>
      <div className="flex items-baseline justify-between font-mono text-[11px]">
        <span className="tabular-nums">{readText || `${offset(position_ms)} of ${fmtDuration(duration_ms)}`}</span>
        <span className="text-muted-foreground">{sel ? 'event' : 'position'}</span>
      </div>
      <div role="img" aria-label={label} className="relative h-8 border bg-background">
        {events.map((e, n) => (
          <span key={n} aria-hidden onMouseEnter={() => setI(n)} onMouseLeave={() => setI(null)}
            className={cn('absolute top-1/2 -translate-x-1/2 -translate-y-1/2 font-mono text-[10px] leading-none', e.state === 'error' ? 'text-destructive' : e.state === 'warn' ? 'text-warning' : 'text-muted-foreground', i === n && 'text-foreground')}
            style={{ left: `${pos(e.at_ms)}%` }}>{KIND_GLYPH[e.kind]}</span>
        ))}
        <span aria-hidden className="absolute inset-y-0 w-px bg-foreground" style={{ left: `${pos(position_ms)}%` }} />
      </div>
      {/* The scrubber is a native range: a keyboard already knows how to drive it. */}
      <label className="flex items-center gap-2 font-mono text-[10px] text-muted-foreground">
        <span className="op-label text-[9px]">position</span>
        <input type="range" min={0} max={duration_ms} step={100} value={position_ms} disabled={!onSeek} aria-label={`position in the session, ${offset(position_ms)} of ${fmtDuration(duration_ms)}`}
          onChange={(e) => onSeek?.(Number(e.target.value))} className="h-1 min-w-0 flex-1 accent-[var(--foreground)]" />
        <span className="tabular-nums text-foreground">{offset(position_ms)}</span>
      </label>
      <ul className="flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">
        {kinds.map((k) => <li key={k} className="flex items-center gap-1"><span aria-hidden>{KIND_GLYPH[k]}</span>{k}</li>)}
      </ul>
      {/* The focusable region wraps the list rather than being it: a `role="group"`
          on the `<ol>` would strip the list semantics from every row. */}
      <div {...regionProps} aria-label={`${events.length} events · arrow keys step, enter seeks`} className={cn(regionProps.className, 'max-h-56 overflow-auto border bg-background')}
        onKeyDown={(e) => { if (e.key === 'Enter' && i !== null) { e.preventDefault(); onSeek?.(events[i].at_ms) } else regionProps.onKeyDown(e) }}>
      <ol className="op-rows font-mono text-[11px]">
        {events.map((e, n) => (
          <li key={n}>
            <button type="button" disabled={!onSeek} onMouseEnter={() => setI(n)} onClick={() => onSeek?.(e.at_ms)}
              className={cn('flex w-full items-baseline gap-3 px-2 py-1 text-left', onSeek && 'hover:bg-muted', (i === n || (i === null && current === n)) && 'bg-muted')}>
              <span className="w-14 shrink-0 tabular-nums text-muted-foreground">{offset(e.at_ms)}</span>
              <span aria-hidden className={cn('w-3 shrink-0 text-center', e.state ? GLYPH_CLASS[e.state] : 'text-muted-foreground')}>{KIND_GLYPH[e.kind]}</span>
              <span className="min-w-0 truncate">{e.label}</span>
              {e.note && <span className="ml-auto shrink-0 truncate text-muted-foreground">{e.note}</span>}
            </button>
          </li>
        ))}
      </ol>
      </div>
      <ReadoutLive text={readText} />
      {footer && <p className="flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">{footer}</p>}
    </div>
  )
}

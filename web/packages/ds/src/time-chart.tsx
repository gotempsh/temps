// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState } from 'react'
import {
  Area,
  ComposedChart,
  Line,
  ReferenceDot,
  ReferenceArea,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip as RechartsTooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { cn } from './lib/cn'
import { fmtAbsolute, fmtNum } from './fmt'
import { Strip } from './datetime'
import { InkPatterns } from './viz-ink'

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
const LABEL_PX = 72
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

const TICK = { fontSize: 10, fill: 'var(--muted-foreground)', fontFamily: 'Geist Mono' }
const TONE = { ok: 'var(--success)', warn: 'var(--warning)', error: 'var(--destructive)' } as const
const DASH: Record<SeriesStroke, string | undefined> = { solid: undefined, dashed: '4 3', dotted: '1 3' }
const AUTO: SeriesStroke[] = ['solid', 'dashed', 'dotted', 'solid']
const strokeOf = (s: Series, i: number): SeriesStroke => s.stroke ?? AUTO[i % AUTO.length]
const widthOf = (s: Series, i: number) => s.width ?? (s.weight ? (s.weight === 'thin' ? 1 : 1.5) : i === 0 ? 1.5 : 1)
const colorOf = (s: Series) => (s.state ? TONE[s.state] : 'var(--foreground)')

/**
 * The legend is generated from `series`, never typed by hand: same order, same
 * name, and a swatch that is a sample of the line itself (same dash, same
 * weight, same ink), so a label can be matched to a line without a hue. The
 * value at the cursor rides the label, so the legend is a readout too.
 */
function Legend({ series, point, unit, compare, delta, band }: { series: Series[]; point: TimePoint | null; unit: string; compare?: Compare; delta?: string; band?: Band }) {
  return (
    <ul className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
      {band && (
        <li className="flex items-center gap-1.5">
          <svg aria-hidden width={18} height={8} viewBox="0 0 18 8" className="shrink-0"><rect x={0} y={0} width={18} height={8} fill="url(#op-hatch-soft)" stroke="var(--op-rule-soft)" /></svg>
          <span>{band.label ?? 'expected range'}</span>
        </li>
      )}
      {series.map((s, i) => (
        <li key={s.key} className="flex items-center gap-1.5">
          <svg aria-hidden width={18} height={6} viewBox="0 0 18 6" className="shrink-0 overflow-visible">
            <line x1={0} y1={3} x2={18} y2={3} stroke={colorOf(s)} strokeWidth={widthOf(s, i)} strokeDasharray={DASH[strokeOf(s, i)]} />
          </svg>
          <span>{s.name}</span>
          {point && point[s.key] !== undefined && (
            <span className="tabular-nums text-foreground">{fmtNum(Number(point[s.key]))}{unit ? ` ${unit}` : ''}</span>
          )}
        </li>
      ))}
      {compare && (
        <li className="flex items-center gap-1.5">
          <svg aria-hidden width={18} height={6} viewBox="0 0 18 6" className="shrink-0 overflow-visible">
            <line x1={0} y1={3} x2={18} y2={3} stroke="var(--muted-foreground)" strokeWidth={1} strokeDasharray="1 3" />
          </svg>
          <span>{compare.label}</span>
          {point && point[PREV] !== undefined && <span className="tabular-nums text-foreground">{fmtNum(Number(point[PREV]))}{unit ? ` ${unit}` : ''}</span>}
          {/* A delta with no baseline is a rumour: the baseline is the compare label itself. */}
          {delta && <span className="text-foreground">{delta} vs {compare.label}</span>}
        </li>
      )}
    </ul>
  )
}

/** The merged key the prior period lands on; never a caller's own key. */
const PREV = '__prev'

/** Is this point outside its own expected range? */
export function outside(p: TimePoint, band: Band, key: string): 0 | 1 | -1 {
  const v = Number(p[key]), lo = Number(p[band.lower]), hi = Number(p[band.upper])
  if (!Number.isFinite(v) || !Number.isFinite(lo) || !Number.isFinite(hi)) return 0
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

function InkTooltip({ active, payload, label }: { active?: boolean; payload?: { name: string; value: number }[]; label?: string }) {
  if (!active || !payload?.length) return null
  return (
    <div className="border bg-popover px-2 py-1.5 font-mono text-[11px]">
      <p className="mb-1 text-muted-foreground">{label}</p>
      {payload.map((p) => <p key={p.name} className="flex justify-between gap-4 tabular-nums"><span className="text-muted-foreground">{p.name}</span><span>{p.value}</span></p>)}
    </div>
  )
}

export function TimeChart({ data: rawData, series, markers = [], thresholds = [], band, anomalies = [], compare, hot, onHot, onOpen, sampled, unit = '', yTicks, height = 176, xInterval, className, readoutFormat, selection: selectionProp, onSelect, legend, table = true, title, range, verdict }: {
  data: TimePoint[]
  series: Series[]
  markers?: Marker[]
  /** Horizontal reference lines (a good/poor threshold). Dashed, labelled at the right edge, coloured by state. */
  thresholds?: { y: number; label: string; state: 'ok' | 'warn' | 'error' }[]
  /**
   * An expected range behind the line, as two keys of the same points. Drawn
   * as a hatched ink band with a generated legend entry — never a filled area,
   * and never a second hue. `BandChart` wraps this for the metrics explorer.
   */
  band?: Band
  /**
   * Points the detector called out of band. Each is a × on the first series'
   * line; the caller lists them under the plot, because a glyph on a plot is
   * not reachable by a keyboard on its own.
   */
  anomalies?: Anomaly[]
  /**
   * The period before this one, point for point (index-aligned). Drawn as a
   * dotted thin ghost, and the legend carries the delta with its baseline
   * ("+9% vs prior 7d"). Compare the same length of window or say nothing.
   */
  compare?: Compare
  hot?: string | null
  onHot?: (id: string | null) => void
  /** Open a deploy from a cluster strip. */
  onOpen?: (id: string) => void
  /** Window during which telemetry was head-sampled. */
  sampled?: { from: string; to: string; label: string }
  unit?: string
  yTicks?: number[]
  height?: number
  xInterval?: number
  className?: string
  /** Custom readout line; defaults to "<t> · <primary> <unit>". */
  readoutFormat?: (p: TimePoint) => string
  /** Controlled selection. Leave undefined for internal state. */
  selection?: TimeRange | null
  /** Called with the selected window, or null when cleared. Enables drag-to-select. */
  onSelect?: (r: TimeRange | null) => void
  /** Generated legend under the plot. Defaults to on whenever there is more than one series. */
  legend?: boolean
  /** The "table" toggle that swaps the plot for the same numbers as rows. On by default. */
  table?: boolean
  /** What the chart is of ("p95 latency"). Goes into the chart's `aria-label`. */
  title?: string
  /** The window shown ("last 24h"). Goes into the chart's `aria-label`. */
  range?: string
  /** The one-sentence verdict a sighted reader takes from the shape. Goes into the `aria-label`. */
  verdict?: string
}) {
  const primaryKey = series[0]?.key
  // The prior period rides on the same points under one reserved key, so the
  // plot, the legend, the readout and the table cannot drift apart.
  const data: TimePoint[] = useMemo(() => {
    if (!compare) return rawData
    return rawData.map((p, i) => {
      const prev = compare.data[i]?.[primaryKey]
      return prev === undefined ? p : { ...p, [PREV]: prev }
    })
  }, [rawData, compare, primaryKey])
  const delta = useMemo(() => {
    if (!compare) return undefined
    const now = rawData.reduce((a, p) => a + (Number(p[primaryKey]) || 0), 0)
    const then = compare.data.reduce((a, p) => a + (Number(p[primaryKey]) || 0), 0)
    if (!then) return undefined
    const pct = ((now - then) / then) * 100
    return `${pct >= 0 ? '+' : ''}${fmtNum(pct, { digits: Math.abs(pct) < 10 ? 1 : 0 })}%`
  }, [compare, rawData, primaryKey])
  const anomalyAt = useMemo(() => new Map(anomalies.map((a) => [a.x, a])), [anomalies])
  const [readout, setReadout] = useState<TimePoint | null>(null)
  const [asTable, setAsTable] = useState(false)
  const [selState, setSelState] = useState<TimeRange | null>(null)
  const selection = selectionProp === undefined ? selState : selectionProp
  const setSelection = (r: TimeRange | null) => { setSelState(r); onSelect?.(r) }
  const [drag, setDrag] = useState<{ from: string; to: string } | null>(null)
  const idxOf = (x: string) => data.findIndex((p) => p.t === x)
  const ordered = (a: string, b: string): TimeRange => (idxOf(a) <= idxOf(b) ? { from: a, to: b } : { from: b, to: a })
  const selBand = drag ? ordered(drag.from, drag.to) : selection
  const selCount = selection ? idxOf(selection.to) - idxOf(selection.from) + 1 : 0
  const selectable = !!onSelect || selectionProp !== undefined
  useEffect(() => {
    if (!selection) return
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setSelection(null) }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection])
  const [openCluster, setOpenCluster] = useState<string | null>(null)
  const wrap = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(600)
  useEffect(() => {
    const el = wrap.current
    if (!el) return
    const ro = new ResizeObserver(([e]) => setWidth(e.contentRect.width))
    ro.observe(el)
    return () => ro.disconnect()
    // re-observe when the plot comes back from the table view: it is a new node.
  }, [asTable])
  // Group markers whose labels would overlap at this width. Plot width ≈ container minus the y axis.
  const clusters = useMemo(() => {
    const pxPerIdx = Math.max(1, (width - 42) / Math.max(1, data.length - 1))
    const idx = (x: string) => data.findIndex((p) => p.t === x)
    const sorted = [...markers].map((m) => ({ m, i: idx(m.x) })).filter((e) => e.i >= 0).sort((a, b) => a.i - b.i)
    const out: { key: string; head: Marker; members: Marker[]; start: number }[] = []
    for (const e of sorted) {
      const cur = out[out.length - 1]
      if (cur && (e.i - cur.start) * pxPerIdx < LABEL_PX) cur.members.push(e.m)
      else out.push({ key: e.m.id, head: e.m, members: [e.m], start: e.i })
    }
    return out
  }, [markers, data, width])
  // The table view carries the deploy markers too, so it says everything the axis says.
  const markerAt = useMemo(() => new Map(markers.map((m) => [m.x, m.id])), [markers])
  const open = clusters.find((c) => c.key === openCluster && c.members.length > 1)
  const last = data[data.length - 1]
  const r = readout ?? last
  const primary = series[0]
  const fmt = readoutFormat ?? ((p: TimePoint) => `${p.t} · ${fmtNum(Number(p[primary.key]))}${unit ? ` ${unit}` : ''}`)
  const showLegend = legend ?? (series.length > 1 || !!compare || !!band)
  const axis = data.length ? `${data[0].t} to ${data[data.length - 1].t}` : 'no points'
  // Every chart is an image with a sentence: what it is, over what window, and
  // the verdict. A caller whose verdict already counts the anomalies (BandChart
  // names the worst one) is not made to say it twice.
  const ariaLabel = `${title ?? series.map((s) => s.name).join(' and ')}${unit ? ` in ${unit}` : ''}, ${range ?? axis}${verdict ? `. ${verdict.replace(/\.\s*$/, '')}` : ''}.${anomalies.length && !/anomal/i.test(verdict ?? '') ? ` ${anomalies.length} point${anomalies.length === 1 ? '' : 's'} outside the expected range.` : ''}${delta && compare ? ` ${delta} vs ${compare.label}.` : ''} ${data.length} points; switch to the table view to read every value.`
  if (import.meta.env.DEV && series.length > 4) console.warn(`[chart] TimeChart has ${series.length} series; more than four lines cannot be told apart by pattern alone. Use small multiples or a table (handoff §8, data-viz.md).`)
  return (
    <div className={cn('space-y-1', className)}>
      {r && (
        <div className="flex items-baseline justify-between font-mono text-[11px]">
          <span className="tabular-nums">{fmt(r)}</span>
          <span className="text-muted-foreground">{readout ? 'hover' : 'latest'}</span>
        </div>
      )}
      {asTable && (
        <div style={{ height }} className="overflow-auto border">
          <table className="w-full font-mono text-[11px]">
            <caption className="sr-only">{ariaLabel}</caption>
            <thead>
              <tr>
                <th scope="col" className="op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-left text-[9px]">bucket</th>
                {series.filter((s) => s.inTable !== false).map((s) => (
                  <th key={s.key} scope="col" className="op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-right text-[9px]">{s.name}{unit ? ` (${unit})` : ''}</th>
                ))}
                {compare && <th scope="col" className="op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-right text-[9px]">{compare.label}{unit ? ` (${unit})` : ''}</th>}
                {band && <th scope="col" className="op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-right text-[9px]">{band.label ?? 'expected'}</th>}
                {band && <th scope="col" className="op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-right text-[9px]">vs expected</th>}
              </tr>
            </thead>
            <tbody className="op-rows">
              {data.map((p) => (
                <tr key={p.t}>
                  <th scope="row" className="whitespace-nowrap px-2 py-1 text-left font-normal text-muted-foreground">
                    {p.t}{markerAt.get(p.t) ? <span className="ml-1.5 text-foreground">┆ {markerAt.get(p.t)}</span> : null}
                  </th>
                  {series.filter((s) => s.inTable !== false).map((s) => (
                    <td key={s.key} className="px-2 py-1 text-right tabular-nums">
                      {p[s.key] === undefined || p[s.key] === null ? '—' : fmtNum(Number(p[s.key]))}
                      {/* The × the plot draws, in the row it belongs to: an anomaly a keyboard can reach. */}
                      {s.key === primaryKey && anomalyAt.has(p.t) && <span className="ml-1.5 text-destructive" title={anomalyAt.get(p.t)?.note}>× out of band</span>}
                    </td>
                  ))}
                  {compare && <td className="px-2 py-1 text-right tabular-nums text-muted-foreground">{p[PREV] === undefined || p[PREV] === null ? '—' : fmtNum(Number(p[PREV]))}</td>}
                  {band && <td className="px-2 py-1 text-right tabular-nums text-muted-foreground">{p[band.lower] === undefined ? '—' : `${fmtNum(Number(p[band.lower]))}–${fmtNum(Number(p[band.upper]))}`}</td>}
                  {/* How far outside, in words: "inside" and "+141% above" are the
                      two facts a reader scans this column for. */}
                  {band && <td className={cn('px-2 py-1 text-right tabular-nums', outside(p, band, primaryKey) ? 'text-destructive' : 'text-muted-foreground')}>{vsExpected(p, band, primaryKey)}</td>}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {band && <InkPatterns />}
      {!asTable && (
      <div ref={wrap} role="img" aria-label={ariaLabel} style={{ height }} className={cn('w-full', selectable && 'select-none', drag && 'cursor-col-resize')} onMouseLeave={() => { setReadout(null); if (drag) { const r = ordered(drag.from, drag.to); setDrag(null); if (r.from !== r.to) setSelection(r) } }}>
        <ResponsiveContainer>
          <ComposedChart data={data} margin={{ top: 16, right: 8, bottom: 0, left: 0 }}
            onMouseDown={(s) => { const x = (s as { activeLabel?: unknown })?.activeLabel; if (selectable && typeof x === 'string') setDrag({ from: x, to: x }) }}
            onMouseUp={() => { if (!drag) return; const r = ordered(drag.from, drag.to); setDrag(null); setSelection(r.from === r.to ? null : r) }}
            onMouseMove={(s) => { const idx = Number((s as { activeIndex?: unknown })?.activeIndex); if (!Number.isNaN(idx) && data[idx]) { setReadout(data[idx]); if (drag) setDrag((d) => (d ? { ...d, to: data[idx].t } : d)) } }}>
            <XAxis dataKey="t" interval={xInterval ?? Math.max(1, Math.floor(data.length / 4) - 1)} tickLine={false} axisLine={{ stroke: 'var(--op-rule-soft)' }} tick={TICK} />
            <YAxis width={34} tickLine={false} axisLine={false} ticks={yTicks} tickFormatter={(v: number) => (v >= 1000 ? `${v / 1000}k` : String(v))} tick={TICK} />
            {selBand && <ReferenceArea x1={selBand.from} x2={selBand.to} fill="var(--foreground)" fillOpacity={0.06} stroke="var(--foreground)" strokeOpacity={0.5} strokeDasharray="2 2" />}
            {sampled && <ReferenceArea x1={sampled.from} x2={sampled.to} fill="var(--muted)" fillOpacity={1} stroke="none" label={{ value: `◌ ${sampled.label}`, position: 'insideBottomRight', fontSize: 10, fill: 'var(--muted-foreground)', fontFamily: 'Geist Mono' }} />}
            {clusters.flatMap((c) => c.members.map((m, j) => {
              const isHead = j === 0
              const many = c.members.length > 1
              const label = many ? `${c.members.length} deploys ${openCluster === c.key ? '▴' : '▾'}` : m.id
              return (
                <ReferenceLine
                  key={m.id}
                  x={m.x}
                  stroke="var(--foreground)"
                  strokeWidth={hot === m.id ? 2 : 1}
                  strokeDasharray={hot === m.id ? undefined : '1 3'}
                  label={isHead ? { value: label, position: 'insideTopLeft', fontSize: 10, fill: 'var(--foreground)', fontFamily: 'Geist Mono', onClick: () => (many ? setOpenCluster((o) => (o === c.key ? null : c.key)) : onHot?.(m.id)), style: { cursor: many || onHot ? 'pointer' : 'default' } } : undefined}
                  onMouseEnter={() => onHot?.(m.id)}
                  onMouseLeave={() => onHot?.(null)}
                />
              )
            }))}
            {/* The expected range sits behind everything: hatched ink, no fill, no hue. */}
            {band && (
              <Area type="linear" dataKey={(d: TimePoint) => [Number(d[band.lower]), Number(d[band.upper])]} name={band.label ?? 'expected range'}
                fill="url(#op-hatch-soft)" fillOpacity={1} stroke="var(--op-rule-soft)" strokeWidth={1} strokeDasharray="2 3" activeDot={false} tooltipType="none" legendType="none" isAnimationActive={false} />
            )}
            {compare && (
              <Line type="linear" dataKey={PREV} name={compare.label} stroke="var(--muted-foreground)" strokeWidth={1} strokeDasharray="1 3" strokeLinecap="square" dot={false} isAnimationActive={false} />
            )}
            {thresholds.map((t) => (
              <ReferenceLine key={t.label} y={t.y} stroke={t.state === 'error' ? 'var(--destructive)' : t.state === 'warn' ? 'var(--warning)' : 'var(--success)'} strokeDasharray="2 3" label={{ value: t.label, position: 'insideRight', fontSize: 10, fill: t.state === 'error' ? 'var(--destructive)' : t.state === 'warn' ? 'var(--warning)' : 'var(--success)', fontFamily: 'Geist Mono' }} />
            ))}
            {/* × on the line, at the value that was out of band. The list under the plot is the keyboard's copy. */}
            {anomalies.map((a) => {
              const at = data.find((p) => p.t === a.x)
              if (!at || at[primaryKey] === undefined) return null
              return (
                <ReferenceDot key={a.x} x={a.x} y={Number(at[primaryKey])} r={0}
                  shape={(props: { cx?: number; cy?: number }) => (
                    <text x={props.cx} y={props.cy} dy={4} textAnchor="middle" fontSize={11} fontFamily="Geist Mono" fill={a.state === 'warn' ? 'var(--warning)' : 'var(--destructive)'}>×</text>
                  )} />
              )
            })}
            <RechartsTooltip content={<InkTooltip />} cursor={{ stroke: 'var(--op-rule-soft)' }} isAnimationActive={false} />
            {/* Drawn back to front so series[0] sits on top, except for a `top`
                series, which is drawn last whatever the legend order. Ink for
                every line; tone only when the series is itself a state. */}
            {series.map((s, i) => ({ s, i })).sort((a, b) => (a.s.top ? 1 : 0) - (b.s.top ? 1 : 0) || b.i - a.i).map(({ s, i }) => (
              <Line key={s.key} type="linear" dataKey={s.key} name={s.name} stroke={colorOf(s)} strokeWidth={widthOf(s, i)} strokeDasharray={DASH[strokeOf(s, i)]} strokeLinecap="square" strokeLinejoin="miter" dot={false} isAnimationActive={false} />
            ))}
          </ComposedChart>
        </ResponsiveContainer>
      </div>
      )}
      {(showLegend || table) && (
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
          {showLegend ? <Legend series={series} point={r ?? null} unit={unit} compare={compare} delta={delta} band={band} /> : <span />}
          {table && (
            <button type="button" aria-pressed={asTable} onClick={() => setAsTable((v) => !v)} className="ml-auto shrink-0 font-mono text-[10px] text-muted-foreground underline underline-offset-4 hover:text-foreground">
              {asTable ? 'chart' : 'table'}
            </button>
          )}
        </div>
      )}
      {selection && !drag && (
        <div className="flex flex-wrap items-center gap-x-3 border px-2 py-1 font-mono text-[11px]">
          <span className="op-label text-[9px]">selected</span>
          <span>{selection.from} → {selection.to}</span>
          <span className="text-muted-foreground">{selCount} of {data.length} points</span>
          <span className="ml-auto text-muted-foreground">{selCount} points in the window</span>
          <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => setSelection(null)}>clear <kbd className="ml-0.5 border px-1 text-[9px]">esc</kbd></button>
        </div>
      )}
      {!selection && !drag && !asTable && selectable && !open && <p className="font-mono text-[10px] text-muted-foreground [@media(pointer:coarse)]:hidden">drag across the plot to select a window</p>}
      {open && (
        <div className="op-rows border font-mono text-[11px]">
          <div className="flex items-center gap-2 px-2 py-1 text-muted-foreground">
            <span>{open.members.length} deploys between {open.members[0].at ?? open.members[0].x} and {open.members[open.members.length - 1].at ?? open.members[open.members.length - 1].x}</span>
            <button type="button" className="ml-auto underline underline-offset-4 hover:text-foreground" onClick={() => setOpenCluster(null)}>close</button>
          </div>
          {open.members.map((m) => (
            <button
              key={m.id}
              type="button"
              onMouseEnter={() => onHot?.(m.id)}
              onMouseLeave={() => onHot?.(null)}
              onClick={() => onOpen?.(m.id)}
              className={cn('flex w-full items-center gap-3 px-2 py-1 text-left hover:bg-muted', hot === m.id && 'bg-muted', !onOpen && 'cursor-default')}
            >
              <span className="w-16 shrink-0">{m.id}</span>
              <span className="w-12 shrink-0 text-muted-foreground">{m.at ?? m.x}</span>
              <span className="min-w-0 truncate">{m.note}</span>
              {onOpen && <span className="ml-auto shrink-0 text-muted-foreground">open ↗</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

/**
 * Range picker with the plan's retention horizon. Ranges beyond it are not
 * hidden: they render struck through and call `onGated` so the page can say
 * which plan keeps that range.
 */
export type Range = { label: string; days: number }
/** The applied window on the strip button: short enough for a 7-cell strip, and always beside the zone in the panel below. */
const fmtWindow = (iso: string) => fmtAbsolute(iso)
/**
 * Quick ranges as one strip; with `custom`, a last button opens two
 * datetime fields under the strip. Once applied the button reads the window
 * ("Sep 5 10:00 → Sep 6 11:00") and `value` is "custom". Ranges beyond the
 * plan's retention are struck through and call `onGated` instead.
 */
export function RangePicker({ ranges, value, onChange, retentionDays, retentionLabel, onGated, custom, className }: {
  ranges: readonly Range[]
  value: string
  onChange: (label: string) => void
  retentionDays: number
  retentionLabel: string
  onGated: (r: Range) => void
  /** Enables the custom window. `from`/`to` are ISO local stamps (datetime-local); `zone` names the clock they are read in, because a time with no zone beside it is a guess. */
  custom?: { from: string; to: string; zone: string; onChange: (from: string, to: string) => void }
  className?: string
}) {
  const wrap = useRef<HTMLDivElement>(null)
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState({ from: custom?.from ?? '', to: custom?.to ?? '' })
  useEffect(() => {
    if (!open) return
    const onDoc = (e: MouseEvent) => { if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false) }
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setOpen(false) }
    document.addEventListener('mousedown', onDoc); window.addEventListener('keydown', onKey)
    return () => { document.removeEventListener('mousedown', onDoc); window.removeEventListener('keydown', onKey) }
  }, [open])
  const isCustom = value === 'custom'
  return (
    <div ref={wrap} className={cn('relative max-w-full', className)}>
    {/* The same Strip a date field's presets and a schedule's weekdays use, so gating is written once (datetime.tsx). */}
    <Strip
      items={ranges.map((r) => {
        const gated = r.days > retentionDays
        return { label: r.label, pressed: value === r.label, gated, title: gated ? `beyond ${retentionLabel} retention` : undefined, onClick: () => (gated ? onGated(r) : onChange(r.label)) }
      })}
      after={custom && (
        <button type="button" aria-pressed={isCustom} aria-expanded={open} onClick={() => setOpen((o) => !o)} className={cn('h-7 shrink-0 border-l px-2 font-mono', isCustom ? 'bg-foreground text-background' : 'hover:bg-muted')}>
          {isCustom && custom.from && custom.to ? `${fmtWindow(custom.from)} → ${fmtWindow(custom.to)}` : 'custom'}
        </button>
      )}
    />
    {custom && open && (
      <form role="dialog" aria-label="Custom range" className="absolute right-0 top-full z-30 mt-1 grid w-[min(22rem,calc(100vw-2rem))] gap-2 border bg-background p-3 text-xs shadow-[3px_3px_0_0_var(--foreground)]"
        onSubmit={(e) => { e.preventDefault(); if (draft.from && draft.to && draft.from < draft.to) { custom.onChange(draft.from, draft.to); onChange('custom'); setOpen(false) } }}>
        {/* The same native inputs the date fields use, under the same ink skin: typed entry first, the browser's picker as the accelerator. */}
        <label className="grid gap-1"><span className="op-label">from</span><input type="datetime-local" required value={draft.from} onChange={(e) => setDraft({ ...draft, from: e.target.value })} className="h-8 border bg-background px-2 font-mono text-xs tabular-nums" /></label>
        <label className="grid gap-1"><span className="op-label">to</span><input type="datetime-local" required value={draft.to} onChange={(e) => setDraft({ ...draft, to: e.target.value })} className="h-8 border bg-background px-2 font-mono text-xs tabular-nums" /></label>
        {/* A time with no zone beside it is a time the reader has to guess at. */}
        <p className="font-mono text-[10px] text-muted-foreground">{draft.from && draft.to && draft.from >= draft.to ? '× "to" must be after "from"' : `retention ${retentionLabel} · times are ${custom.zone}`}</p>
        <div className="flex justify-end gap-2"><button type="button" onClick={() => setOpen(false)} className="h-7 px-2 hover:bg-muted">cancel</button><button type="submit" className="op-primary h-7 border px-3">apply</button></div>
      </form>
    )}
    </div>
  )
}

/** Footer under a TimeChart: what is shown, the retention, the legend. */
export function ChartFooter({ children }: { children: React.ReactNode }) {
  return <p className="flex flex-wrap items-center gap-x-3 font-mono text-[10px] text-muted-foreground">{children}</p>
}

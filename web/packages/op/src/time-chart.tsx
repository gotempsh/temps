// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState } from 'react'
import {
  ComposedChart,
  Line,
  ReferenceArea,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip as RechartsTooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { cn } from './lib/cn'
import { fmtAbsolute, fmtNum } from './fmt'

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
}

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
function Legend({ series, point, unit }: { series: Series[]; point: TimePoint | null; unit: string }) {
  return (
    <ul className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
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
    </ul>
  )
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

export function TimeChart({ data, series, markers = [], thresholds = [], hot, onHot, onOpen, sampled, unit = '', yTicks, height = 176, xInterval, className, readoutFormat, selection: selectionProp, onSelect, legend, table = true, title, range, verdict }: {
  data: TimePoint[]
  series: Series[]
  markers?: Marker[]
  /** Horizontal reference lines (a good/poor threshold). Dashed, labelled at the right edge, coloured by state. */
  thresholds?: { y: number; label: string; state: 'ok' | 'warn' | 'error' }[]
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
  const [readout, setReadout] = useState<TimePoint | null>(null)
  const [asTable, setAsTable] = useState(false)
  const [selState, setSelState] = useState<TimeRange | null>(null)
  const selection = selectionProp === undefined ? selState : selectionProp
  const setSelection = (r: TimeRange | null) => { setSelState(r); onSelect?.(r) }
  const [drag, setDrag] = useState<{ from: string; to: string } | null>(null)
  const idxOf = (x: string) => data.findIndex((p) => p.t === x)
  const ordered = (a: string, b: string): TimeRange => (idxOf(a) <= idxOf(b) ? { from: a, to: b } : { from: b, to: a })
  const band = drag ? ordered(drag.from, drag.to) : selection
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
  const showLegend = legend ?? series.length > 1
  const axis = data.length ? `${data[0].t} to ${data[data.length - 1].t}` : 'no points'
  // Every chart is an image with a sentence: what it is, over what window, and the verdict.
  const ariaLabel = `${title ?? series.map((s) => s.name).join(' and ')}${unit ? ` in ${unit}` : ''}, ${range ?? axis}${verdict ? `. ${verdict.replace(/\.\s*$/, '')}` : ''}. ${data.length} points; switch to the table view to read every value.`
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
                {series.map((s) => (
                  <th key={s.key} scope="col" className="op-label sticky top-0 z-10 border-b bg-background px-2 py-1 text-right text-[9px]">{s.name}{unit ? ` (${unit})` : ''}</th>
                ))}
              </tr>
            </thead>
            <tbody className="op-rows">
              {data.map((p) => (
                <tr key={p.t}>
                  <th scope="row" className="whitespace-nowrap px-2 py-1 text-left font-normal text-muted-foreground">
                    {p.t}{markerAt.get(p.t) ? <span className="ml-1.5 text-foreground">┆ {markerAt.get(p.t)}</span> : null}
                  </th>
                  {series.map((s) => (
                    <td key={s.key} className="px-2 py-1 text-right tabular-nums">{p[s.key] === undefined || p[s.key] === null ? '—' : fmtNum(Number(p[s.key]))}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {!asTable && (
      <div ref={wrap} role="img" aria-label={ariaLabel} style={{ height }} className={cn('w-full', selectable && 'select-none', drag && 'cursor-col-resize')} onMouseLeave={() => { setReadout(null); if (drag) { const r = ordered(drag.from, drag.to); setDrag(null); if (r.from !== r.to) setSelection(r) } }}>
        <ResponsiveContainer>
          <ComposedChart data={data} margin={{ top: 16, right: 8, bottom: 0, left: 0 }}
            onMouseDown={(s) => { const x = (s as { activeLabel?: unknown })?.activeLabel; if (selectable && typeof x === 'string') setDrag({ from: x, to: x }) }}
            onMouseUp={() => { if (!drag) return; const r = ordered(drag.from, drag.to); setDrag(null); setSelection(r.from === r.to ? null : r) }}
            onMouseMove={(s) => { const idx = Number((s as { activeIndex?: unknown })?.activeIndex); if (!Number.isNaN(idx) && data[idx]) { setReadout(data[idx]); if (drag) setDrag((d) => (d ? { ...d, to: data[idx].t } : d)) } }}>
            <XAxis dataKey="t" interval={xInterval ?? Math.max(1, Math.floor(data.length / 4) - 1)} tickLine={false} axisLine={{ stroke: 'var(--op-rule-soft)' }} tick={TICK} />
            <YAxis width={34} tickLine={false} axisLine={false} ticks={yTicks} tickFormatter={(v: number) => (v >= 1000 ? `${v / 1000}k` : String(v))} tick={TICK} />
            {band && <ReferenceArea x1={band.from} x2={band.to} fill="var(--foreground)" fillOpacity={0.06} stroke="var(--foreground)" strokeOpacity={0.5} strokeDasharray="2 2" />}
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
            {thresholds.map((t) => (
              <ReferenceLine key={t.label} y={t.y} stroke={t.state === 'error' ? 'var(--destructive)' : t.state === 'warn' ? 'var(--warning)' : 'var(--success)'} strokeDasharray="2 3" label={{ value: t.label, position: 'insideRight', fontSize: 10, fill: t.state === 'error' ? 'var(--destructive)' : t.state === 'warn' ? 'var(--warning)' : 'var(--success)', fontFamily: 'Geist Mono' }} />
            ))}
            <RechartsTooltip content={<InkTooltip />} cursor={{ stroke: 'var(--op-rule-soft)' }} isAnimationActive={false} />
            {/* Drawn back to front so series[0] sits on top; ink for every line, tone only when the series is itself a state. */}
            {series.map((s, i) => ({ s, i })).reverse().map(({ s, i }) => (
              <Line key={s.key} type="linear" dataKey={s.key} name={s.name} stroke={colorOf(s)} strokeWidth={widthOf(s, i)} strokeDasharray={DASH[strokeOf(s, i)]} strokeLinecap="square" strokeLinejoin="miter" dot={false} isAnimationActive={false} />
            ))}
          </ComposedChart>
        </ResponsiveContainer>
      </div>
      )}
      {(showLegend || table) && (
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
          {showLegend ? <Legend series={series} point={r ?? null} unit={unit} /> : <span />}
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
const fmtStamp = (iso: string) => fmtAbsolute(iso)
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
  /** Enables the custom window. `from`/`to` are ISO local stamps (datetime-local). */
  custom?: { from: string; to: string; onChange: (from: string, to: string) => void }
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
    <div className="op-scroll-x flex max-w-full border text-[11px]">
      {ranges.map((r, i) => {
        const gated = r.days > retentionDays
        return (
          <button
            key={r.label}
            type="button"
            aria-pressed={value === r.label}
            title={gated ? `beyond ${retentionLabel} retention` : undefined}
            onClick={() => (gated ? onGated(r) : onChange(r.label))}
            className={cn('h-7 shrink-0 px-2', i > 0 && 'border-l', value === r.label ? 'bg-foreground text-background' : gated ? 'text-muted-foreground line-through decoration-[var(--op-rule-soft)] hover:bg-muted' : 'hover:bg-muted')}
          >
            {r.label}
          </button>
        )
      })}
      {custom && (
        <button type="button" aria-pressed={isCustom} aria-expanded={open} onClick={() => setOpen((o) => !o)} className={cn('h-7 shrink-0 border-l px-2 font-mono', isCustom ? 'bg-foreground text-background' : 'hover:bg-muted')}>
          {isCustom && custom.from && custom.to ? `${fmtStamp(custom.from)} → ${fmtStamp(custom.to)}` : 'custom'}
        </button>
      )}
    </div>
    {custom && open && (
      <form role="dialog" aria-label="Custom range" className="absolute right-0 top-full z-30 mt-1 grid w-[min(22rem,calc(100vw-2rem))] gap-2 border bg-background p-3 text-xs shadow-[3px_3px_0_0_var(--foreground)]"
        onSubmit={(e) => { e.preventDefault(); if (draft.from && draft.to && draft.from < draft.to) { custom.onChange(draft.from, draft.to); onChange('custom'); setOpen(false) } }}>
        <label className="grid gap-1"><span className="op-label">from</span><input type="datetime-local" required value={draft.from} onChange={(e) => setDraft({ ...draft, from: e.target.value })} className="h-8 border bg-background px-2 font-mono text-xs" /></label>
        <label className="grid gap-1"><span className="op-label">to</span><input type="datetime-local" required value={draft.to} onChange={(e) => setDraft({ ...draft, to: e.target.value })} className="h-8 border bg-background px-2 font-mono text-xs" /></label>
        <p className="font-mono text-[10px] text-muted-foreground">{draft.from && draft.to && draft.from >= draft.to ? '× "to" must be after "from"' : `retention ${retentionLabel} · times are local`}</p>
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

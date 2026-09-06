// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ComposableMap, Geographies, Geography } from 'react-simple-maps'
import worldTopo from './assets/geo/countries-110m.json'
import { useMemo, useRef, useState, type ReactNode } from 'react'
import { ChevronRight } from 'lucide-react'
import { cn } from './lib/cn'
import { GLYPH, GLYPH_CLASS, type State } from './status'
import { Num } from './num'
import { fmtNum, fmtPct } from './fmt'
import { Kbd } from './kbd'

/* ────────────────────────────────────────────────────────────────────────
   The observe primitives the live console draws and the system did not have
   (docs/console-inventory.md). Every one of them obeys the same rules as a
   Ledger row: ink on paper, mono tabular numbers, colour only through the
   five states, one soft rule between rows, an ink frame around the group.
   None of them needs a library: they are SVG and CSS.
   ──────────────────────────────────────────────────────────────────────── */

const TONE: Record<State, string> = { ok: 'bg-success', warn: 'bg-warning', error: 'bg-destructive', idle: 'bg-muted-foreground/40', sampled: 'bg-[repeating-linear-gradient(135deg,transparent_0_2px,var(--op-rule-soft)_2px_4px)]' }

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
export type BreakdownRow = { label: ReactNode; key?: string; count: number; state?: State; /** What kind of thing the row is (a flag, a browser mark, a channel icon). Drawn in a fixed 16px slot so labels align. */ icon?: ReactNode; children?: BreakdownRow[]; onOpen?: () => void }
export function Breakdown({ rows, total, unit = 'visitors', limit = 8, more, percent = true, className }: { rows: BreakdownRow[]; total: number; unit?: string; limit?: number; /** "view all" link for the full dimension page. */ more?: { label: string; onClick: () => void }; /** Off when `count` is a measurement (ms, bytes) rather than a count: then there is no share to print and no "other" remainder. */ percent?: boolean; className?: string }) {
  const [path, setPath] = useState<BreakdownRow[]>([])
  const current = path.length ? path[path.length - 1].children ?? [] : rows
  const shown = current.slice(0, limit)
  const sum = shown.reduce((a, r) => a + r.count, 0)
  const denom = path.length ? current.reduce((a, r) => a + r.count, 0) : total
  const rest = percent ? Math.max(0, denom - sum) : 0
  const max = Math.max(1, ...shown.map((r) => r.count))
  return (
    <div className={cn('op-breakdown flex h-full min-w-0 flex-col border bg-background text-xs', className)}>
      {path.length > 0 && (
        <div className="flex items-center gap-1 border-b px-3 py-1.5 text-[11px] text-muted-foreground">
          <a href="#" onClick={(e) => { e.preventDefault(); setPath([]) }} className="hover:text-foreground">all</a>
          {path.map((p, i) => (
            <span key={i} className="flex items-center gap-1">
              <span aria-hidden className="text-[var(--op-rule-soft)]">/</span>
              {i < path.length - 1 ? <a href="#" onClick={(e) => { e.preventDefault(); setPath(path.slice(0, i + 1)) }} className="hover:text-foreground">{p.label}</a> : <span className="text-foreground">{p.label}</span>}
            </span>
          ))}
        </div>
      )}
      <ol className="op-rows">
        {shown.map((r, i) => {
          const share = denom ? (r.count / denom) * 100 : 0
          const openable = !!r.children?.length || !!r.onOpen
          const open = () => { if (r.children?.length) setPath([...path, r]); else r.onOpen?.() }
          return (
            <li key={r.key ?? i} className="relative">
              <span aria-hidden className="absolute inset-y-1 left-0 bg-foreground/[0.06]" style={{ width: `${(r.count / max) * 100}%` }} />
              <button type="button" disabled={!openable} onClick={open} className={cn('relative grid w-full grid-cols-[minmax(0,1fr)_auto_3.5rem] items-center gap-3 px-3 py-1.5 text-left', openable && 'hover:bg-muted/60')}>
                <span className="flex min-w-0 items-center gap-1.5">
                  {r.icon && <span aria-hidden className="flex w-4 shrink-0 items-center justify-center text-muted-foreground [&_svg]:h-3.5 [&_svg]:w-3.5">{r.icon}</span>}
                  {r.state && <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[r.state])}>{GLYPH[r.state]}</span>}
                  <span className="min-w-0 truncate">{r.label}</span>
                  {openable && <ChevronRight aria-hidden className="h-3 w-3 shrink-0 text-muted-foreground" />}
                </span>
                <Num value={r.count} unit={percent ? undefined : unit} />
                <span className="text-right font-mono tabular-nums text-muted-foreground">{percent ? fmtPct(share, { digits: share < 10 ? 1 : 0 }) : ''}</span>
              </button>
            </li>
          )
        })}
        {rest > 0 && (
          <li className="grid grid-cols-[minmax(0,1fr)_auto_3.5rem] items-center gap-3 px-3 py-1.5 text-muted-foreground">
            <span>other</span><Num value={rest} /><span className="text-right font-mono tabular-nums">{fmtPct(rest / denom, { basis: 'ratio', digits: 0 })}</span>
          </li>
        )}
      </ol>
      <div className="mt-auto flex items-center justify-between border-t px-3 py-1.5 text-[11px] text-muted-foreground">
        <span>{shown.length} of {current.length}{percent && <> · <Num value={denom} /> {unit}</>}</span>
        {more && !path.length && <a href="#" onClick={(e) => { e.preventDefault(); more.onClick() }}>{more.label}</a>}
      </div>
    </div>
  )
}

// ── Sparkline ──────────────────────────────────────────────────────────

/**
 * A trend in the width of a cell: one ink line, no axes, no dots, the last
 * point marked. It never carries a number; the number sits beside it in
 * mono. Width follows the container, height is 20px by default. `state`
 * colours the line only for warn/error.
 */
export function Sparkline({ points, height = 20, state, fill, className }: { points: number[]; height?: number; state?: State; fill?: boolean; className?: string }) {
  const w = 100
  const max = Math.max(...points, 1), min = Math.min(...points, 0)
  const xs = points.map((_, i) => (i / Math.max(1, points.length - 1)) * w)
  const ys = points.map((p) => height - 1 - ((p - min) / Math.max(1e-9, max - min)) * (height - 2))
  const d = xs.map((x, i) => `${i ? 'L' : 'M'}${x.toFixed(1)},${ys[i].toFixed(1)}`).join(' ')
  const stroke = state === 'error' ? 'var(--destructive)' : state === 'warn' ? 'var(--warning)' : 'currentColor'
  return (
    <svg viewBox={`0 0 ${w} ${height}`} preserveAspectRatio="none" className={cn('block h-5 w-full', className)} style={{ height }} aria-hidden>
      {fill && <path d={`${d} L${w},${height} L0,${height} Z`} fill="currentColor" opacity={0.08} />}
      <path d={d} fill="none" stroke={stroke} strokeWidth={1.25} vectorEffect="non-scaling-stroke" />
      <circle cx={xs[xs.length - 1]} cy={ys[ys.length - 1]} r={1.5} fill={stroke} vectorEffect="non-scaling-stroke" />
    </svg>
  )
}

// ── StatusStrip ────────────────────────────────────────────────────────

/**
 * Uptime over a window: one segment per bucket, coloured by its state, the
 * legend is the five glyphs. Hover or focus a segment to read it (start,
 * state, checks, p50/p95). The strip is the whole width of its cell so the
 * reader compares monitors by shape, not by number.
 */
export type StatusBucket = { start: string; state: State; checks?: number; down?: number; p50_ms?: number; p95_ms?: number }
export function StatusStrip({ buckets, height = 20, className }: { buckets: StatusBucket[]; height?: number; className?: string }) {
  const [hover, setHover] = useState<number | null>(null)
  const b = hover !== null ? buckets[hover] : null
  return (
    <div className={cn('relative min-w-0', className)}>
      {/* One focusable strip; arrows move a reader through the buckets so the per-bucket data is reachable without a mouse. */}
      <div className="flex gap-px outline-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring" style={{ height }} role="group" tabIndex={0} aria-label={`${buckets.length} buckets · ${buckets.filter((b) => b.state === 'error').length} down · use arrow keys to read each`}
        onFocus={() => setHover((h) => h ?? buckets.length - 1)} onBlur={() => setHover(null)}
        onKeyDown={(e) => { if (e.key === 'ArrowLeft') { e.preventDefault(); setHover((h) => Math.max(0, (h ?? buckets.length) - 1)) } if (e.key === 'ArrowRight') { e.preventDefault(); setHover((h) => Math.min(buckets.length - 1, (h ?? -1) + 1)) } }}>
        {buckets.map((bk, i) => (
          <span key={i} aria-hidden onMouseEnter={() => setHover(i)} onMouseLeave={() => setHover(null)} className={cn('min-w-0 flex-1', TONE[bk.state], hover === i && 'ring-1 ring-foreground')} />
        ))}
      </div>
      <span className="sr-only" aria-live="polite">{b ? `${b.start} ${b.state}${b.checks !== undefined ? `, ${b.checks} checks${b.down ? `, ${b.down} down` : ''}` : ''}${b.p95_ms !== undefined ? `, p95 ${b.p95_ms}ms` : ''}` : ''}</span>
      {b && (
        <div className="pointer-events-none absolute left-0 top-full z-20 mt-1 whitespace-nowrap border bg-background px-2 py-1 font-mono text-[11px] shadow-[3px_3px_0_var(--foreground)]" style={{ left: `${((hover ?? 0) / buckets.length) * 100}%`, transform: hover !== null && hover > buckets.length / 2 ? 'translateX(-100%)' : undefined }}>
          <span className="text-muted-foreground">{b.start}</span> <span className={GLYPH_CLASS[b.state]}>{GLYPH[b.state]}</span> {b.state}
          {b.checks !== undefined && <> · {b.checks} checks{b.down ? `, ${b.down} down` : ''}</>}
          {b.p50_ms !== undefined && <> · p50 {b.p50_ms}ms</>}{b.p95_ms !== undefined && <> · p95 {b.p95_ms}ms</>}
        </div>
      )}
    </div>
  )
}

// ── ScoreRing ──────────────────────────────────────────────────────────

/**
 * A 0–100 score as an arc. The number is the message and sits in the
 * middle in mono; the arc is a shape for scanning a row of them. Colour is
 * the state word derived from thresholds (≥90 ok, ≥50 warn, else error),
 * never a gradient.
 */
export function ScoreRing({ value, size = 56, label, className }: { value: number; size?: number; label?: string; className?: string }) {
  const state: State = value >= 90 ? 'ok' : value >= 50 ? 'warn' : 'error'
  const r = (size - 6) / 2, c = 2 * Math.PI * r
  const stroke = state === 'ok' ? 'var(--success)' : state === 'warn' ? 'var(--warning)' : 'var(--destructive)'
  return (
    <span className={cn('inline-flex flex-col items-center gap-1', className)}>
      <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} role="img" aria-label={`${label ?? 'score'} ${value}`}>
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--op-rule-soft)" strokeWidth={3} />
        <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke={stroke} strokeWidth={3} strokeDasharray={`${(value / 100) * c} ${c}`} strokeLinecap="butt" transform={`rotate(-90 ${size / 2} ${size / 2})`} />
        <text x="50%" y="50%" dominantBaseline="central" textAnchor="middle" className="fill-foreground font-mono text-sm tabular-nums">{Math.round(value)}</text>
      </svg>
      {label && <span className="op-label">{label}</span>}
    </span>
  )
}

// ── CalendarHeatmap ────────────────────────────────────────────────────

/**
 * Activity per day over weeks: a grid of 12px cells, columns are weeks,
 * rows are weekdays, five ink intensities. Ink, not green: the colour of a
 * cell is how much, not how well. Hover reads date and count.
 */
export type ActivityDay = { date: string; count: number }
export function CalendarHeatmap({ days, cell = 12, className }: { days: ActivityDay[]; cell?: number; className?: string }) {
  const max = Math.max(1, ...days.map((d) => d.count))
  const weeks: ActivityDay[][] = []
  days.forEach((d, i) => { if (i % 7 === 0) weeks.push([]); weeks[weeks.length - 1].push(d) })
  const level = (n: number) => (n === 0 ? 0 : Math.min(4, 1 + Math.floor((n / max) * 3.999)))
  const tones = ['bg-foreground/[0.06]', 'bg-foreground/25', 'bg-foreground/45', 'bg-foreground/70', 'bg-foreground']
  return (
    <div className={cn('inline-flex flex-col gap-2', className)}>
      <div className="flex gap-0.5" role="img" aria-label={`${days.length} days`}>
        {weeks.map((w, i) => (
          <div key={i} className="flex flex-col gap-0.5">
            {w.map((d) => <span key={d.date} title={`${d.date} · ${d.count}`} className={cn('block', tones[level(d.count)])} style={{ width: cell, height: cell }} />)}
          </div>
        ))}
      </div>
      <div className="flex items-center gap-1 self-end text-[11px] text-muted-foreground">less {tones.map((t) => <span key={t} className={cn('block', t)} style={{ width: 8, height: 8 }} />)} more</div>
    </div>
  )
}

// ── Funnel ─────────────────────────────────────────────────────────────

/**
 * Steps of a funnel as bars whose width is the share of entrants still
 * present; under each, completions, conversion from the previous step, and
 * drop-off. Ink bars; the drop-off is the number that matters and is the
 * only thing that can turn red (above `dropAlert`).
 */
export type FunnelStep = { name: string; count: number; avgSeconds?: number }
export function Funnel({ steps, dropAlert = 50, className }: { steps: FunnelStep[]; dropAlert?: number; className?: string }) {
  const first = steps[0]?.count || 1
  return (
    <ol className={cn('op-rows border bg-background text-xs', className)}>
      {steps.map((s, i) => {
        const prev = i ? steps[i - 1].count : s.count
        const conv = prev ? (s.count / prev) * 100 : 0
        const drop = 100 - conv
        return (
          <li key={s.name} className="grid grid-cols-[1.5rem_minmax(0,1fr)_auto] items-center gap-3 px-3 py-2">
            <span className="font-mono text-[11px] text-muted-foreground">{i + 1}</span>
            <span className="min-w-0">
              <span className="flex items-baseline justify-between gap-3"><span className="truncate font-medium">{s.name}</span><span className="font-mono tabular-nums"><Num value={s.count} /> <span className="text-muted-foreground">{fmtPct(s.count / first, { basis: 'ratio', digits: 0 })}</span></span></span>
              <span className="mt-1 block h-2 bg-foreground/[0.06]"><span className="block h-2 bg-foreground" style={{ width: `${(s.count / first) * 100}%` }} /></span>
            </span>
            <span className="w-28 text-right font-mono text-[11px] tabular-nums">
              {i === 0 ? <span className="text-muted-foreground">entered</span> : <>
                <span className="text-muted-foreground">{fmtPct(conv, { digits: 0 })} on · </span>
                <span className={drop >= dropAlert ? 'text-destructive' : 'text-muted-foreground'}>{drop >= dropAlert && <span aria-hidden>× </span>}{fmtPct(drop, { digits: 0 })} off</span>
              </>}
              {s.avgSeconds !== undefined && <span className="block text-muted-foreground">{s.avgSeconds < 60 ? `${s.avgSeconds}s` : `${Math.round(s.avgSeconds / 60)}m`} avg</span>}
            </span>
          </li>
        )
      })}
    </ol>
  )
}

// ── Flow ───────────────────────────────────────────────────────────────

/**
 * Transitions between pages: "from → to", count, share of the from-page's
 * exits. Not a Sankey: a ranked list is readable, sortable, and honest at
 * any width. Entry, exit and drop-off lists are the same rows with one
 * side empty.
 */
export type FlowRow = { from?: string; to?: string; count: number; share: number }
export function Flow({ rows, className }: { rows: FlowRow[]; className?: string }) {
  const max = Math.max(1, ...rows.map((r) => r.count))
  return (
    <ol className={cn('op-rows border bg-background font-mono text-xs', className)}>
      {rows.map((r, i) => (
        <li key={i} className="relative">
          <span aria-hidden className="absolute inset-y-1 left-0 bg-foreground/[0.06]" style={{ width: `${(r.count / max) * 100}%` }} />
          <span className="relative grid grid-cols-[minmax(0,1fr)_1.5rem_minmax(0,1fr)_auto_3rem] items-center gap-2 px-3 py-1.5">
            <span className={cn('truncate', !r.from && 'text-muted-foreground')}>{r.from ?? '(entry)'}</span>
            <span aria-hidden className="text-center text-muted-foreground">→</span>
            <span className={cn('truncate', !r.to && 'text-muted-foreground')}>{r.to ?? '(exit)'}</span>
            <Num value={r.count} />
            <span className="text-right tabular-nums text-muted-foreground">{fmtPct(r.share, { digits: 0 })}</span>
          </span>
        </li>
      ))}
    </ol>
  )
}

// ── Waterfall ──────────────────────────────────────────────────────────

/**
 * Spans of one trace: a tree on the left (collapsible), the bar on the
 * right placed by offset and width against the trace duration, the
 * duration in mono at the bar's end. Error spans get the × glyph and a red
 * bar; everything else is ink. Selecting a row is the caller's business
 * (`onSelect`), typically opening the span's attributes beside it.
 */
export type Span = { id: string; name: string; service?: string; start_ms: number; duration_ms: number; state?: State; children?: Span[] }
export function Waterfall({ spans, total_ms, selected, onSelect, className }: { spans: Span[]; total_ms: number; selected?: string; onSelect?: (s: Span) => void; className?: string }) {
  const [closed, setClosed] = useState<Set<string>>(new Set())
  const rows = useMemo(() => {
    const out: { s: Span; depth: number; hasKids: boolean }[] = []
    const walk = (list: Span[], depth: number) => list.forEach((s) => { out.push({ s, depth, hasKids: !!s.children?.length }); if (s.children?.length && !closed.has(s.id)) walk(s.children, depth + 1) })
    walk(spans, 0); return out
  }, [spans, closed])
  const fmt = (ms: number) => (ms < 1 ? `${(ms * 1000).toFixed(0)}µs` : ms < 1000 ? `${ms.toFixed(ms < 10 ? 1 : 0)}ms` : `${(ms / 1000).toFixed(2)}s`)
  return (
    <div className={cn('op-waterfall min-w-0 border bg-background font-mono text-[11px]', className)}>
      <div className="grid grid-cols-[minmax(12rem,2fr)_minmax(0,3fr)_4rem] gap-x-3 border-b px-3 py-1 text-muted-foreground"><span className="op-label">span</span><span className="op-label">0 → {fmt(total_ms)}</span><span className="op-label text-right">took</span></div>
      <ol className="op-rows">
        {rows.map(({ s, depth, hasKids }) => (
          <li key={s.id} className={cn('grid grid-cols-[minmax(12rem,2fr)_minmax(0,3fr)_4rem] items-center gap-3 px-3 py-1 hover:bg-muted/60', selected === s.id && 'bg-muted')}>
            <span className="flex min-w-0 items-center gap-1" style={{ paddingLeft: depth * 12 }}>
              {hasKids ? <button type="button" aria-label={closed.has(s.id) ? 'expand' : 'collapse'} onClick={() => setClosed((c) => { const n = new Set(c); if (n.has(s.id)) n.delete(s.id); else n.add(s.id); return n })} className="-my-1 inline-flex h-7 w-5 shrink-0 items-center justify-center text-muted-foreground hover:text-foreground"><ChevronRight className={cn('h-3 w-3 transition-transform', !closed.has(s.id) && 'rotate-90')} /></button> : <span className="w-3" />}
              {s.state === 'error' && <span aria-hidden className={GLYPH_CLASS.error}>{GLYPH.error}</span>}
              <button type="button" onClick={() => onSelect?.(s)} className="min-w-0 truncate text-left hover:underline">{s.name}</button>
              {s.service && <span className="truncate text-muted-foreground">{s.service}</span>}
            </span>
            <span className="relative h-4 min-w-0">
              <span className={cn('absolute inset-y-0.5', s.state === 'error' ? 'bg-destructive' : 'bg-foreground')} style={{ left: `${(s.start_ms / total_ms) * 100}%`, width: `max(2px, ${(s.duration_ms / total_ms) * 100}%)` }} />
            </span>
            <span className={cn('text-right tabular-nums', s.state === 'error' ? 'text-destructive' : 'text-muted-foreground')}>{fmt(s.duration_ms)}</span>
          </li>
        ))}
      </ol>
    </div>
  )
}

// ── StackTrace ─────────────────────────────────────────────────────────

/**
 * Frames of one error, most recent first. In-app frames are ink and open
 * by default with their source context (line numbers in the gutter, the
 * failing line marked); vendor frames are muted and closed. A frame that
 * was symbolicated shows the original file in the gutter's corner. This is
 * the Sentry frame list without the card per frame.
 */
export type Frame = { fn: string; file: string; line: number; col?: number; inApp?: boolean; original?: string; context?: { line: number; code: string }[] }
export function StackTrace({ frames, className }: { frames: Frame[]; className?: string }) {
  const [open, setOpen] = useState<Set<number>>(() => new Set(frames.map((f, i) => (f.inApp && f.context ? i : -1)).filter((i) => i >= 0)))
  return (
    <ol className={cn('op-rows border bg-background font-mono text-[11px]', className)}>
      {frames.map((f, i) => {
        const isOpen = open.has(i)
        return (
          <li key={i} className={cn(!f.inApp && 'text-muted-foreground')}>
            <button type="button" disabled={!f.context} aria-expanded={f.context ? isOpen : undefined} onClick={() => setOpen((o) => { const n = new Set(o); if (n.has(i)) n.delete(i); else n.add(i); return n })}
              className={cn('grid w-full grid-cols-[1rem_minmax(0,1fr)_auto] items-center gap-2 px-3 py-1.5 text-left', f.context && 'hover:bg-muted/60')}>
              {f.context ? <ChevronRight className={cn('h-3 w-3 transition-transform', isOpen && 'rotate-90')} /> : <span />}
              <span className="min-w-0 truncate"><span className={cn(f.inApp && 'font-medium text-foreground')}>{f.fn}</span> <span className="text-muted-foreground">in</span> {f.file}</span>
              <span className="tabular-nums text-muted-foreground">:{f.line}{f.col !== undefined && `:${f.col}`}{f.original && <span className="ml-2 border px-1 text-[10px]">map · {f.original}</span>}</span>
            </button>
            {isOpen && f.context && (
              /* A source excerpt scrolls sideways on a narrow column, so it is
                 focusable: a scrollable region a keyboard cannot reach is
                 content a keyboard user cannot read. */
              <pre tabIndex={0} className="op-inset overflow-x-auto border-t border-[var(--op-rule-soft)] py-1 text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
                {f.context.map((c) => (
                  <div key={c.line} className={cn('grid grid-cols-[3rem_minmax(0,1fr)] gap-3 px-3', c.line === f.line && 'bg-destructive/10')}>
                    <span className={cn('select-none text-right tabular-nums', c.line === f.line ? 'text-destructive' : 'text-muted-foreground')}>{c.line === f.line && <span aria-hidden>× </span>}{c.line}</span>
                    <span className="whitespace-pre text-foreground">{c.code}</span>
                  </div>
                ))}
              </pre>
            )}
          </li>
        )
      })}
    </ol>
  )
}

// ── LogLines · Stages ──────────────────────────────────────────────────

/**
 * Lines of a log: time in the gutter, level as a glyph (error ×, warn ◐,
 * everything else nothing), source in muted, the message in mono and
 * wrapping. A level filter is a row of toggles above; the count of hidden
 * lines is said. `live` pins the newest line at the bottom and says so.
 * Virtualisation is the console's job; this is the row.
 */
export type LogLine = { t: string; level: 'error' | 'warn' | 'info' | 'debug'; source?: string; msg: string }
const LEVEL_STATE: Record<LogLine['level'], State | null> = { error: 'error', warn: 'warn', info: null, debug: null }
export function LogLines({ lines, live, height = 240, search, className }: { lines: LogLine[]; live?: boolean; height?: number; /** A text filter in the toolbar; for a full build or runtime log, where the reader is looking for one line. */ search?: boolean; className?: string }) {
  const [levels, setLevels] = useState<Set<LogLine['level']>>(new Set(['error', 'warn', 'info', 'debug']))
  const [q, setQ] = useState('')
  const needle = q.trim().toLowerCase()
  const shown = lines.filter((l) => levels.has(l.level) && (!needle || l.msg.toLowerCase().includes(needle) || (l.source ?? '').toLowerCase().includes(needle)))
  const toggle = (l: LogLine['level']) => setLevels((s) => { const n = new Set(s); if (n.has(l)) n.delete(l); else n.add(l); return n })
  const count = (l: LogLine['level']) => lines.filter((x) => x.level === l).length
  return (
    <div className={cn('op-log min-w-0 border bg-background font-mono text-[11px]', className)}>
      <div className="flex flex-wrap items-center gap-1 border-b px-2 py-1">
        {(['error', 'warn', 'info', 'debug'] as const).map((l) => (
          <button key={l} type="button" aria-pressed={levels.has(l)} onClick={() => toggle(l)} className={cn('inline-flex h-6 items-center gap-1 border px-2', levels.has(l) ? 'bg-muted' : 'text-muted-foreground line-through')}>
            {LEVEL_STATE[l] && <span aria-hidden className={GLYPH_CLASS[LEVEL_STATE[l]!]}>{GLYPH[LEVEL_STATE[l]!]}</span>}{l} <span className="text-muted-foreground">{count(l)}</span>
          </button>
        ))}
        {search && <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="find in log" aria-label="find in log" className="h-6 min-w-0 flex-1 basis-32 border bg-background px-2 font-mono text-[11px] outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring sm:max-w-56" />}
        <span className="ml-auto text-muted-foreground">{shown.length} of {lines.length}{live && <> · <span className="text-success">● live</span></>}</span>
      </div>
      <ol className="op-inset overflow-y-auto" style={{ maxHeight: height }}>
        {shown.map((l, i) => (
          <li key={i} className="grid grid-cols-[4.5rem_1rem_minmax(0,1fr)] gap-2 px-3 py-0.5 leading-5 hover:bg-muted/60 sm:grid-cols-[4.5rem_1rem_7rem_minmax(0,1fr)]">
            <span className="tabular-nums text-muted-foreground">{l.t}</span>
            <span aria-hidden className={cn('text-center', LEVEL_STATE[l.level] && GLYPH_CLASS[LEVEL_STATE[l.level]!])}>{LEVEL_STATE[l.level] ? GLYPH[LEVEL_STATE[l.level]!] : ''}</span>
            <span className="hidden truncate text-muted-foreground sm:block">{l.source}</span>
            <span className={cn('min-w-0 whitespace-pre-wrap break-words', l.level === 'error' && 'text-destructive', l.level === 'debug' && 'text-muted-foreground')}>{l.msg}</span>
          </li>
        ))}
        {shown.length === 0 && <li className="px-3 py-3 text-muted-foreground">{needle ? <>no line matches &quot;{q}&quot; · <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => setQ('')}>clear</button></> : 'nothing at these levels'}</li>}
      </ol>
    </div>
  )
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
export function Stages({ stages, className }: { stages: Stage[]; className?: string }) {
  const running = stages.findIndex((s) => s.state === 'idle' && s.lines)
  const [open, setOpen] = useState<number>(running >= 0 ? running : stages.findIndex((s) => s.state === 'error'))
  const pending = (i: number) => running >= 0 && i > running
  return (
    // One grid for the whole list (subgrid rows): the name track is as wide as the widest name, so every result starts on the same left edge instead of floating after its own name.
    <ol className={cn('op-rows border bg-background text-xs sm:grid sm:grid-cols-[1.5rem_1rem_max-content_minmax(0,1fr)_auto]', className)}>
      {stages.map((s, i) => (
        <li key={s.name} className="sm:col-span-full sm:grid sm:grid-cols-subgrid">
          {s.phase && s.phase !== stages[i - 1]?.phase && <p className={cn('op-label border-b border-[var(--op-rule-soft)] px-3 py-1 sm:col-span-full', i > 0 && 'border-t')}>{s.phase}</p>}
          <button type="button" disabled={!s.lines} aria-expanded={s.lines ? open === i : undefined} onClick={() => setOpen((o) => (o === i ? -1 : i))} className={cn('grid w-full grid-cols-[1.5rem_1rem_minmax(0,1fr)_auto] items-center gap-x-3 px-3 py-2 text-left sm:col-span-full sm:grid-cols-subgrid', s.lines && 'hover:bg-muted/60', pending(i) && 'text-muted-foreground')}>
            <span className="font-mono text-[11px] text-muted-foreground">{i + 1}</span>
            <span aria-hidden className={cn('text-center', GLYPH_CLASS[s.state])}>{GLYPH[s.state]}</span>
            <span className="min-w-0 truncate font-medium">{s.name}{s.state === 'idle' && s.lines && <span className="ml-2 font-normal text-muted-foreground">running…</span>}</span>
            <span className={cn('col-span-full col-start-3 min-w-0 truncate font-mono text-[11px] sm:col-span-1 sm:col-start-auto', s.state === 'error' ? 'text-destructive' : 'text-muted-foreground')}>{s.result ?? ''}</span>
            <span className="col-start-4 row-start-1 inline-flex items-center justify-end gap-1.5 font-mono text-[11px] tabular-nums text-muted-foreground sm:col-start-auto sm:row-start-auto">{s.duration ?? ''}{s.lines && <ChevronRight aria-hidden className={cn('h-3 w-3 transition-transform', open === i && 'rotate-90')} />}</span>
          </button>
          {open === i && s.lines && <LogLines lines={s.lines} live={s.state === 'idle'} height={200} className="border-x-0 border-b-0 border-t border-[var(--op-rule-soft)] sm:col-span-full" />}
        </li>
      ))}
    </ol>
  )
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
const PCTS = ['avg', 'p50', 'p90', 'p95', 'p99'] as const
export type Pct = (typeof PCTS)[number]
export function quantile(buckets: HistBucket[], q: number) {
  const total = buckets.reduce((a, b) => a + b.count, 0)
  let acc = 0, prevLe = 0
  for (const b of buckets) {
    if (acc + b.count >= q * total) { const need = q * total - acc; return prevLe + (need / Math.max(1, b.count)) * (b.le - prevLe) }
    acc += b.count; prevLe = b.le
  }
  return prevLe
}
export function Histogram({ buckets, unit = 'ms', value, onChange, height = 96, className }: { buckets: HistBucket[]; unit?: string; value: Pct; onChange: (p: Pct) => void; height?: number; className?: string }) {
  const total = buckets.reduce((a, b) => a + b.count, 0)
  const max = Math.max(1, ...buckets.map((b) => b.count))
  const stat = value === 'avg' ? buckets.reduce((a, b, i) => a + b.count * ((i ? buckets[i - 1].le : 0) + b.le) / 2, 0) / Math.max(1, total) : quantile(buckets, Number(value.slice(1)) / 100)
  const last = buckets[buckets.length - 1]?.le ?? 1
  return (
    <div className={cn('op-hist min-w-0 border bg-background text-xs', className)}>
      <div className="flex flex-wrap items-center justify-between gap-2 border-b px-3 py-1.5">
        <div className="flex border text-[11px]">
          {PCTS.map((p, i) => <button key={p} type="button" aria-pressed={value === p} onClick={() => onChange(p)} className={cn('h-6 px-2 font-mono', i > 0 && 'border-l', value === p ? 'bg-foreground text-background' : 'hover:bg-muted')}>{p}</button>)}
        </div>
        <span className="font-mono tabular-nums">{value} <span className="font-medium">{fmtNum(stat, stat < 10 ? { digits: 1 } : { digits: 0 })}</span><span className="text-muted-foreground">{unit}</span> <span className="text-muted-foreground">· <Num value={total} /> samples</span></span>
      </div>
      <div className="relative px-3 pb-5 pt-2">
        <div className="flex items-end gap-px" style={{ height }}>
          {buckets.map((b, i) => <span key={i} title={`≤${b.le}${unit} · ${b.count}`} className={cn('min-w-0 flex-1', b.le <= stat ? 'bg-foreground' : 'bg-foreground/25')} style={{ height: `${(b.count / max) * 100}%` }} />)}
        </div>
        <span aria-hidden className="absolute bottom-5 top-2 w-px bg-destructive" style={{ left: `calc(0.75rem + (100% - 1.5rem) * ${Math.min(1, stat / last)})` }} />
        <div className="mt-1 flex justify-between font-mono text-[10px] text-muted-foreground"><span>0</span><span>{fmtNum(last)}{unit}</span></div>
      </div>
    </div>
  )
}

// ── LiveDot ────────────────────────────────────────────────────────────

/** "This updates by itself": a green glyph, the word, and the interval. Sits in a section's meta or a ledger footer. */
export function Live({ every, paused, onToggle }: { every: string; paused?: boolean; onToggle?: () => void }) {
  return (
    <button type="button" onClick={onToggle} disabled={!onToggle} className="inline-flex items-center gap-1.5 font-mono text-[11px]" aria-pressed={!paused}>
      <span aria-hidden className={paused ? 'text-muted-foreground' : 'text-success'}>{paused ? GLYPH.idle : GLYPH.ok}</span>
      {paused ? 'paused' : 'live'} <span className="text-muted-foreground">· every {every}</span>
      {onToggle && <Kbd keys="space" />}
    </button>
  )
}


// ── GeoMap: a choropleth by state, the second view of a "by country" list ──
export type GeoRow = { /** Name as it appears in the topojson (e.g. "United States of America"). */ geo: string; label: string; value: string; state: State; note?: string }
const GEO_FILL: Record<State, string> = {
  ok: 'color-mix(in oklch, var(--success) 38%, var(--background))',
  warn: 'color-mix(in oklch, var(--warning) 55%, var(--background))',
  error: 'color-mix(in oklch, var(--destructive) 60%, var(--background))',
  idle: 'var(--muted)', sampled: 'var(--muted)',
}
/**
 * Countries filled by state, nothing else: no gradient, no legend of ten
 * bins. On a fine pointer the hovered country reads at the pointer and
 * nothing sits under the map; clicking opens it. Below md (touch) the
 * readout is a row under the map: tap a country to read it, tap it again to
 * open. Countries without data are the muted tone. The list is the primary
 * view and carries the keyboard; the map is the second view of the same rows.
 */
export function GeoMap({ rows, onOpen, className }: { rows: GeoRow[]; onOpen?: (row: GeoRow) => void; className?: string }) {
  const [hot, setHot] = useState<GeoRow | null>(null)
  const [at, setAt] = useState<{ x: number; y: number } | null>(null)
  const box = useRef<HTMLDivElement>(null)
  const byGeo = useMemo(() => Object.fromEntries(rows.map((r) => [r.geo, r])), [rows])
  // A real touch fires touchstart before the synthesised click; the media query covers devices that never touch.
  const touched = useRef(false)
  const coarse = () => touched.current || (typeof window !== 'undefined' && window.matchMedia('(pointer: coarse)').matches)
  const readout = hot ? (
    <>
      {hot.state !== 'idle' && <span aria-hidden className={GLYPH_CLASS[hot.state]}>{GLYPH[hot.state]}</span>}
      <span className="text-foreground">{hot.label}</span>
      {hot.value ? <span className="text-muted-foreground">· {hot.value}{hot.note ? ` · ${hot.note}` : ''}</span> : <span className="text-muted-foreground">· no samples</span>}
    </>
  ) : null
  const text = hot ? `${hot.label} · ${hot.value || 'no samples'}${hot.note ? ` · ${hot.note}` : ''}` : ''
  return (
    <div ref={box} className={cn('op-geo relative min-w-0 border bg-background', className)} onTouchStart={() => { touched.current = true }}>
      <ComposableMap projection="geoNaturalEarth1" projectionConfig={{ scale: 155, center: [0, 8] }} width={880} height={400} style={{ width: '100%', height: 'auto', display: 'block' }} role="img" aria-label={`map · ${rows.length} countries with data`}
        onMouseMove={(e) => { const r = box.current?.getBoundingClientRect(); if (r) setAt({ x: e.clientX - r.left, y: e.clientY - r.top }) }}>
        <Geographies geography={worldTopo}>
          {({ geographies }) => geographies.filter((g) => g.id !== '010').map((g) => {
            const row = byGeo[g.properties.name as string]
            const read = row ?? { geo: g.properties.name, label: g.properties.name, value: '', state: 'idle' as State }
            return (
              <Geography key={g.rsmKey} geography={g}
                fill={row ? GEO_FILL[row.state] : GEO_FILL.idle}
                stroke={hot && hot.geo === read.geo ? 'var(--foreground)' : 'var(--background)'} strokeWidth={hot && hot.geo === read.geo ? 1 : 0.5}
                style={{ default: { outline: 'none' }, hover: { outline: 'none', stroke: 'var(--foreground)', strokeWidth: 1, cursor: row && onOpen ? 'pointer' : 'default' }, pressed: { outline: 'none' } }}
                onMouseEnter={() => { if (!coarse()) setHot(read) }}
                onMouseLeave={() => { if (!coarse()) { setHot(null); setAt(null) } }}
                onClick={() => {
                  if (!coarse()) { if (row) onOpen?.(row); return }
                  // Touch: first tap reads the country in the row under the map, a second tap on it opens.
                  if (hot?.geo === read.geo) { if (row) onOpen?.(row) } else setHot(read)
                }} />
            )
          })}
        </Geographies>
      </ComposableMap>
      {/* fine pointer: the readout follows the cursor, nothing under the map */}
      {hot && at && (
        <div aria-hidden className="pointer-events-none absolute z-10 hidden items-center gap-2 border bg-background px-2 py-1 font-mono text-[11px] md:flex"
          style={{ left: at.x + 12, top: at.y + 12, ...(box.current && at.x > box.current.clientWidth * 0.6 ? { left: 'auto', right: box.current.clientWidth - at.x + 12 } : {}) }}>
          {readout}
        </div>
      )}
      {/* touch: the readout is a row under the map */}
      <div aria-hidden className="flex min-h-[1.75rem] items-center gap-2 border-t px-3 py-1.5 font-mono text-[11px] md:hidden">
        {readout ?? <span className="text-muted-foreground">tap a country to read it · tap it again to filter</span>}
      </div>
      <span className="sr-only" aria-live="polite">{text}</span>
    </div>
  )
}

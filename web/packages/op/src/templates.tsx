// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Children, isValidElement, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { useShellSlots } from './shell-slots'
import { Search } from 'lucide-react'
import { Button } from './ui/button'
import { Input } from './ui/input'
import { cn } from './lib/cn'
import { Kbd } from './kbd'
import { GLYPH, GLYPH_CLASS, Status, type State } from './status'

/* ────────────────────────────────────────────────────────────────────────
   The three page templates. Every console screen is one of these. A screen
   that does not fit is a reason to extend a template, not to start from a
   blank div.

   Ledger    title · status line · filter (/) · actions · rows with j/k/⏎ · footer
   Detail    title · status line · tabs with number keys · actions · body
   Settings  title · status line · sections · sticky save (⌘S) · danger zone
   ──────────────────────────────────────────────────────────────────────── */

// ── PageTitle ──────────────────────────────────────────────────────────

/**
 * Every screen starts with what it is: the title is the only 700-weight text
 * on a console screen, with the one or two facts that place it (environment,
 * current deploy, image, region) in mono beside it. Where it is belongs to
 * the shell header: inside a shell the title is also portalled into the
 * header breadcrumb as the current crumb (ancestors are the shell's), so a
 * detail page's trail ends in the resource's real name, never its id.
 * Outside a shell a `crumbs` prop renders the trail above the title. The
 * block carries its own top padding: it is the first thing under the header
 * and needs air, not a border, to separate from it.
 */
export type Crumb = { label: ReactNode; onClick?: () => void }

/** A title's identity mark (a project favicon, a provider logo) goes in `mark`, never inside `title` as a flex box: a flex box's baseline is its first item, so a mark-first title drags the meta down to the mark's bottom edge instead of the text's baseline. */
export function PageTitle({ title, meta, crumbs, mark, className }: { title: ReactNode; meta?: ReactNode; crumbs?: Crumb[]; mark?: ReactNode; className?: string }) {
  const slots = useShellSlots()
  const trail = slots?.crumb ? [] : (crumbs ?? [])
  return (
    <div className={cn('min-w-0 pt-5', className)}>
      {slots?.crumb && createPortal(<><span aria-hidden className="text-[var(--op-rule-soft)]">/</span><span aria-current="page" className="truncate text-foreground">{title}</span></>, slots.crumb)}
      {trail.length > 0 && (
        <nav aria-label="Breadcrumb" className="mb-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          {trail.map((c, i) => (
            <span key={i} className="flex min-w-0 items-center gap-1.5">
              {i > 0 && <span aria-hidden className="text-[var(--op-rule-soft)]">/</span>}
              {c.onClick ? <a href="#" onClick={(e) => { e.preventDefault(); c.onClick?.() }} className="truncate hover:text-foreground">{c.label}</a> : <span className="truncate">{c.label}</span>}
            </span>
          ))}
          <span aria-hidden className="text-[var(--op-rule-soft)]">/</span>
          <span aria-current="page" className="truncate text-foreground">{title}</span>
        </nav>
      )}
      <div className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <h1 className="op-title min-w-0 truncate">{mark && <span aria-hidden className="mr-2 inline-flex translate-y-[-0.1em] align-middle [&>*]:shrink-0">{mark}</span>}{title}</h1>
        {meta && <p className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{meta}</p>}
      </div>
    </div>
  )
}

// ── Ledger ─────────────────────────────────────────────────────────────

// ── SectionTitle ───────────────────────────────────────────────────────

/**
 * The title of a section inside a page. `.op-h3` (1rem, 600) with the count
 * or one fact in mono beside it and an optional action on the right. This is
 * the tier between the page title (700) and row text (400); without it a page
 * of `.op-label` eyebrows has no hierarchy and reads as one grey column.
 * `.op-label` is for column headers, field names, eyebrows and key badges,
 * never for the title of a section.
 */
export function SectionTitle({ title, meta, action, className }: { title: ReactNode; meta?: ReactNode; action?: ReactNode; className?: string }) {
  return (
    <div className={cn('flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1', className)}>
      <h2 className="min-w-0 truncate text-sm font-semibold leading-6 tracking-[-0.01em]">{title}</h2>
      {meta && <span className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">{meta}</span>}
      {action && <span className="ml-auto text-xs">{action}</span>}
    </div>
  )
}

// ── Section · KeyValue · Timeline: the anatomy of a record page ────────

/**
 * The summary block: the one raised element on a record page and the first
 * thing the eye lands on. Top line is the record's state as glyph + word at
 * 600/18px with one muted sentence (when, to whom, via what). Under a soft
 * rule, `facts` are the four to six values the reader will want without
 * scrolling (to, from, provider, project), each a small muted key over a mono
 * value. One per page; everything below it is detail. The header's attention
 * count is the shell's copy of the same verdict; the lede is the page's.
 */
export function Lede({ state, word, children, facts, className }: { state: State; word: ReactNode; children?: ReactNode; facts?: KV[]; className?: string }) {
  if (import.meta.env.DEV && (!facts || facts.length < 3)) console.warn(`[record recipe] Lede "${String(word)}" has ${facts?.length ?? 0} facts; a lede carries the four to six facts the reader wants without scrolling (handoff §7, record page). Put them in \`facts\`, not in the aside.`)
  return (
    <div className={cn('op-raise min-w-0 border bg-background', className)}>
      <p className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1 px-4 py-3">
        <span className="inline-flex items-center gap-2 text-lg font-semibold leading-7 tracking-[-0.01em]">
          <span aria-hidden className={cn('text-base', GLYPH_CLASS[state])}>{GLYPH[state]}</span>{word}
        </span>
        {children && <span className="min-w-0 text-sm text-muted-foreground">{children}</span>}
      </p>
      {facts && facts.length > 0 && (
        <dl className="grid grid-cols-2 gap-x-6 gap-y-3 border-t border-[var(--op-rule-soft)] px-4 py-3 text-xs sm:grid-cols-3 lg:grid-cols-[repeat(auto-fit,minmax(10rem,1fr))]">
          {facts.map((f) => (
            <div key={f.k} className="min-w-0">
              <dt className="op-label">{f.k}</dt>
              <dd className={cn('mt-0.5 flex min-w-0 items-baseline gap-1.5', f.mono !== false && 'font-mono')}>
                {f.state && <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[f.state])}>{GLYPH[f.state]}</span>}
                <span className="min-w-0 truncate">{f.v}</span>
              </dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  )
}

/**
 * A record page body: the main column (content, then what happened to it)
 * and, at xl, a narrow aside on the right for reference facts (headers,
 * identifiers) at a smaller size. Below xl the aside stacks under the main
 * column with the same ink rule a `Section` would draw.
 */
export function Columns({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('op-halves grid gap-x-10 xl:grid-cols-[minmax(0,1fr)_18rem]', className)}>{children}</div>
}

/**
 * A section of a page: `SectionTitle` and one body (a KeyValue, a Timeline,
 * a Ledger, a chart, prose). Sections stacked in one column separate with an
 * ink rule and 1.25rem of air above and below (CSS sibling rule, so the first
 * never gets one and nobody has to pass `first`). Two independent halves of a
 * record go in a two-column grid at xl, each column its own stack.
 */
export function Section({ title, meta, action, children, className }: { title: ReactNode; meta?: ReactNode; action?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={cn('op-block flex min-w-0 flex-col', className)}>
      <SectionTitle title={title} meta={meta} action={action} />
      <div className="op-block-body min-w-0 flex-1">{children}</div>
    </section>
  )
}

/**
 * Facts about one record, the grouped-list way: one row per fact, soft rule
 * between rows, the key on the left in muted 400 at a fixed 11rem, the value
 * on the right in ink (mono for identifiers, addresses, ids). Values wrap;
 * keys never do. `copy` puts a copy button after the value.
 */
export type KV = { k: string; v: ReactNode; mono?: boolean; copy?: string; state?: State }
export function KeyValue({ rows, compact, className }: { rows: KV[]; /** Key over value, 11px: for the narrow aside of a record page. */ compact?: boolean; className?: string }) {
  return (
    <dl className={cn('op-kv', compact ? 'text-[11px]' : 'text-xs', className)}>
      {rows.map((r) => (
        <div key={r.k} className={cn('grid grid-cols-1 gap-x-4 gap-y-0.5 px-3 py-2', !compact && 'sm:grid-cols-[11rem_minmax(0,1fr)]')}>
          <dt className="text-muted-foreground">{r.k}</dt>
          <dd className={cn('flex min-w-0 items-baseline gap-2', r.mono !== false && 'font-mono')}>
            {r.state && <span aria-hidden className={cn('w-3 shrink-0 text-center', r.state === 'ok' ? 'text-success' : r.state === 'warn' ? 'text-warning' : r.state === 'error' ? 'text-destructive' : 'text-muted-foreground')}>{r.state === 'ok' ? '●' : r.state === 'warn' ? '◐' : r.state === 'error' ? '×' : '○'}</span>}
            <span className="min-w-0 break-words">{r.v}</span>
          </dd>
        </div>
      ))}
    </dl>
  )
}

/**
 * What happened to one record, in order, as a vertical rail. Each event is
 * drawn by an icon that says what kind of event it was (queued, sent,
 * delivered, opened, bounced…), never by a coloured dot: a dot only says
 * "fine/not fine", the icon says what. The icon turns red for `state`
 * error and muted for idle/sampled; otherwise it is ink. The label is the
 * event word at 500, the note explains it at 400 muted, the time sits right
 * in mono. Callers own the icon vocabulary (see `MAIL_EVENT_ICONS` in the
 * email page) so the same event always gets the same icon across pages.
 */
export type TimelineItem = { t: string; label: string; icon?: ReactNode; state?: State; note?: ReactNode }
export function Timeline({ items, className }: { items: TimelineItem[]; className?: string }) {
  return (
    <ol className={cn('op-timeline text-xs', className)}>
      {items.map((e, i) => {
        const tone = e.state === 'error' ? 'text-destructive' : e.state === 'warn' ? 'text-warning' : e.state === 'idle' || e.state === 'sampled' ? 'text-muted-foreground' : 'text-foreground'
        return (
          <li key={i} className="relative grid grid-cols-[1.5rem_minmax(0,1fr)_auto] gap-x-3 px-3 py-2.5">
            {i < items.length - 1 && <span aria-hidden className="absolute bottom-0 left-[1.5rem] top-[1.9rem] w-px bg-[var(--op-rule-soft)]" />}
            <span aria-hidden className={cn('relative z-10 flex h-6 w-6 items-center justify-center border bg-background [&_svg]:h-3.5 [&_svg]:w-3.5', tone, !e.icon && 'text-[10px]')}>
              {e.icon ?? (e.state ? GLYPH[e.state] : '·')}
            </span>
            <span className="min-w-0 pt-0.5">
              <span className="font-medium">{e.label}</span>
              {e.note && <span className="block min-w-0 text-muted-foreground">{e.note}</span>}
            </span>
            <span className="pt-0.5 font-mono text-[11px] text-muted-foreground">{e.t}</span>
          </li>
        )
      })}
    </ol>
  )
}

// ── ActionBar ──────────────────────────────────────────────────────────

/**
 * The actions of a page or list. From sm up: a right-aligned row. On a phone
 * a row of 3–5 buttons wraps into a ragged pile, so below sm the bar becomes
 * a two-column grid of full-width controls; anything that is not a button or
 * link (RangePicker, Picker, Segmented) takes the whole row. Order is kept,
 * so the primary action stays last and lands bottom-right, under the thumb.
 */
/**
 * The tab row. Facets never wrap: on a phone the strip scrolls sideways, the
 * active tab is scrolled into view, and a fade on the clipped edge says there
 * is more. Ten facets fit the same way six do. If a record needs more than
 * about seven, some of them are tools (logs, data, terminal) that belong in
 * the actions, not facets.
 */
function ScrollRow({ children, className, role }: { children: ReactNode; className?: string; role?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const [edge, setEdge] = useState<{ l: boolean; r: boolean }>({ l: false, r: false })
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const update = () => setEdge({ l: el.scrollLeft > 1, r: el.scrollLeft + el.clientWidth < el.scrollWidth - 1 })
    update()
    el.addEventListener('scroll', update, { passive: true })
    const ro = new ResizeObserver(update)
    ro.observe(el)
    return () => { el.removeEventListener('scroll', update); ro.disconnect() }
  }, [])
  return (
    <div className={cn('relative min-w-0 max-w-full', className)}>
      <div ref={ref} role={role} className="op-scroll-x flex">{children}</div>
      {edge.l && <span aria-hidden className="pointer-events-none absolute inset-y-px left-px w-6 bg-gradient-to-r from-background to-transparent" />}
      {edge.r && <span aria-hidden className="pointer-events-none absolute inset-y-px right-px w-6 bg-gradient-to-l from-background to-transparent" />}
    </div>
  )
}

/**
 * Actions. From sm up a right-aligned wrapping row. Below sm the same row at
 * natural widths, scrolling sideways with an edge fade when it does not fit
 * (the same form as the tab strip), so three actions are three compact
 * buttons on one line and never a pile of full-width bars or a grid with a
 * hole. Order is kept; the primary is last.
 */
export function ActionBar({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <ScrollRow className={cn('op-actions w-full sm:w-auto', className)}>
      <div className="flex w-full items-center gap-2 sm:flex-wrap sm:justify-end [&>*]:shrink-0">{children}</div>
    </ScrollRow>
  )
}

// ── Pager ──────────────────────────────────────────────────────────────

/**
 * Pagination, one way everywhere. Server-side, page-numbered, matching the
 * API (default 20 per page, max 100). Lives in a list's footer, never as a
 * bar of numbered buttons:
 *   1–20 of 1,284 · ‹ prev · next › · 20 per page
 * The range is the fact, prev/next are the only moves (the filter and the
 * sort are for finding things, not paging to them), and the page size is a
 * plain select with the API's allowed values. `[` and `]` page from the
 * keyboard while a ledger has the focus. Filtering or sorting resets to
 * page 1, which the caller does in `onFilter` / `onSort`. Infinite scroll is
 * banned: an operator needs to say "page 3" to a colleague.
 */
export type Page = { page: number; pageSize: number; total: number; onPage: (page: number) => void; onPageSize?: (size: number) => void; sizes?: readonly number[] }
export const PAGE_SIZES = [20, 50, 100] as const

export function Pager({ page: p, className }: { page: Page; className?: string }) {
  const pages = Math.max(1, Math.ceil(p.total / p.pageSize))
  const from = p.total === 0 ? 0 : (p.page - 1) * p.pageSize + 1
  const to = Math.min(p.total, p.page * p.pageSize)
  const btn = 'inline-flex h-7 items-center gap-0.5 px-1.5 hover:bg-muted hover:text-foreground disabled:opacity-40 disabled:hover:bg-transparent'
  return (
    <span className={cn('inline-flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground', className)}>
      <span className="tabular-nums"><span className="text-foreground">{from.toLocaleString()}–{to.toLocaleString()}</span> of {p.total.toLocaleString()}</span>
      <span aria-hidden>·</span>
      <button type="button" className={btn} disabled={p.page <= 1} onClick={() => p.onPage(p.page - 1)} aria-label="previous page"><span aria-hidden>‹</span> prev</button>
      <button type="button" className={btn} disabled={p.page >= pages} onClick={() => p.onPage(p.page + 1)} aria-label="next page">next <span aria-hidden>›</span></button>
      {pages > 1 && <span className="tabular-nums">page {p.page} of {pages}</span>}
      {p.onPageSize && (
        <label className="inline-flex items-center gap-1"><select value={p.pageSize} onChange={(e) => p.onPageSize?.(Number(e.target.value))} aria-label="rows per page" className="h-5 border bg-background px-1 font-mono text-[11px] text-foreground">{(p.sizes ?? PAGE_SIZES).map((n) => <option key={n} value={n}>{n}</option>)}</select> per page</label>
      )}
    </span>
  )
}

export type LedgerRow = {
  id: string
  state: State
  /** Desktop cells, one per column. Use <Num>, <Status>, plain spans. */
  cells: ReactNode[]
  /** Phone rendering: name on the first line, the status note on the second. */
  mobile: ReactNode
  onOpen?: () => void
  /** Raw values per sortable column key. Numbers sort numerically, null sorts last. */
  sort?: Record<string, string | number | null | undefined>
}

/**
 * A column is a label, or a label with a sort key. Clicking a sortable header
 * cycles asc → desc → off (back to the ledger's default order, which is the
 * `hint`). Exactly one column sorts at a time; there is no multi-sort, the
 * filter box is for narrowing. Numeric columns right-align.
 */
export type LedgerColumn = string | { label: string; key?: string; numeric?: boolean }
export type LedgerSort = { key: string; dir: 'asc' | 'desc' } | null

export function Ledger({ title, meta, status, columns, grid, rows, total, filter, onFilter, placeholder = 'filter', hint, action, dense, state, footer, sort: sortProp, onSort, defaultSort = null, page }: {
  title?: ReactNode
  meta?: ReactNode
  status: ReactNode
  columns: LedgerColumn[]
  /** CSS grid-template-columns for md+, e.g. "1.4fr 1fr 140px 80px". */
  grid: string
  rows: LedgerRow[]
  total: number
  /** A drawn control is a wired control: omit `filter`/`onFilter` and the search box and its `/` binding are not rendered at all. */
  filter?: string
  onFilter?: (q: string) => void
  placeholder?: string
  /** Sort explanation, e.g. "needs attention first, then last deploy". */
  hint?: ReactNode
  action?: ReactNode
  dense: boolean
  /** A <PageState> to render instead of rows. */
  state?: ReactNode
  footer?: ReactNode
  /** Controlled sort. Leave undefined for internal state. */
  sort?: LedgerSort
  onSort?: (s: LedgerSort) => void
  defaultSort?: LedgerSort
  /** Server-side pagination. `rows` are the current page; the footer shows range, prev/next, page size. `[` `]` page. */
  page?: Page
}) {
  const [cursor, setCursor] = useState(0)
  // A drawn control is a wired control: no filter props means no search box, and no `/` binding to focus one.
  const filterable = filter !== undefined && !!onFilter
  const [sortState, setSortState] = useState<LedgerSort>(defaultSort)
  const sort = sortProp === undefined ? sortState : sortProp
  const setSort = (s: LedgerSort) => { setSortState(s); onSort?.(s); setCursor(0) }
  const cols_ = columns.map((c) => (typeof c === 'string' ? { label: c } : c))
  // A paged ledger only holds one page of rows: sorting them client-side would reorder 20 of N and claim
  // the set was sorted. Sorting a paged ledger therefore needs the server (`onSort`); without it the headers are labels.
  const sortable = !page || !!onSort
  const cycle = (key: string) => setSort(!sort || sort.key !== key ? { key, dir: 'asc' } : sort.dir === 'asc' ? { key, dir: 'desc' } : null)
  const sorted = useMemo(() => {
    if (!sort || (page && !onSort)) return rows
    const k = sort.key
    const val = (r: LedgerRow) => r.sort?.[k]
    return [...rows].sort((a, b) => {
      const av = val(a), bv = val(b)
      if (av == null && bv == null) return 0
      if (av == null) return 1 // nulls last, whatever the direction
      if (bv == null) return -1
      const c = typeof av === 'number' && typeof bv === 'number' ? av - bv : String(av).localeCompare(String(bv), undefined, { numeric: true, sensitivity: 'base' })
      return sort.dir === 'asc' ? c : -c
    })
  }, [rows, sort])
  const listRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  // Truncated cells get their full text as a title, so the clipped tail of a path or address is one hover away.
  useEffect(() => {
    listRef.current?.querySelectorAll<HTMLElement>('.truncate').forEach((el) => { if (el.scrollWidth > el.clientWidth + 1) { if (!el.title) el.title = el.textContent ?? '' } else if (el.title === el.textContent) el.removeAttribute('title') })
  }, [sorted, dense])
  const focusRow = (i: number) => {
    setCursor(i)
    listRef.current?.querySelectorAll<HTMLElement>('.op-row[tabindex]')[i]?.focus()
  }
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || e.metaKey || e.ctrlKey) return
      // vim/Gmail/GitHub convention: j is down, k is up. Arrow keys do the same for everyone else.
      // The cursor IS the focus: moving it focuses the row, so Enter always acts on the row the bar marks
      // (the row's own onKeyDown opens it). A cursor that only paints while focus sits on a link elsewhere
      // makes Enter follow that link and strands the reader.
      if (e.key === 'j' || e.key === 'ArrowDown') { e.preventDefault(); focusRow(Math.min(sorted.length - 1, cursor + 1)) }
      else if (e.key === 'k' || e.key === 'ArrowUp') { e.preventDefault(); focusRow(Math.max(0, cursor - 1)) }
      else if (page && e.key === ']' && page.page < Math.ceil(page.total / page.pageSize)) { page.onPage(page.page + 1); setCursor(0) }
      else if (page && e.key === '[' && page.page > 1) { page.onPage(page.page - 1); setCursor(0) }
      else if (e.key === 'Enter' && e.target === document.body) sorted[cursor]?.onOpen?.()
      else if (filterable && e.key === '/') { e.preventDefault(); inputRef.current?.focus() }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [sorted, cursor, page, filterable])

  // `1fr` means minmax(auto, 1fr): one unbreakable 90-character name would widen the
  // column and the whole ledger. Clamp every fr track to minmax(6rem, …) so long cells
  // truncate; if the fixed tracks plus the floors still exceed the container (a ledger
  // in a narrow column) the rows scroll sideways instead of breaking the page.
  // Track vocabulary: `Nfr` for the long text columns (they truncate), `NNpx` only for
  // numbers of known width, `minmax(NNpx, max-content)` for short text of varying
  // length (cadence, source, state words): those grow to fit the widest row instead
  // of truncating, because the list is one grid with subgrid rows.
  const cols = { '--cols': grid.replace(/(?<![\w(,.])(\d*\.?\d+)fr\b/g, 'minmax(6rem,$1fr)') } as CSSProperties
  return (
    <div className="space-y-4">
      {title && <PageTitle title={title} meta={meta} />}
      {status}
      {(filterable || hint || action) && <div className="flex flex-wrap items-center gap-2">
        {filterable && (
          <div className="relative min-w-0 flex-1 basis-40 sm:max-w-72">
            <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input ref={inputRef} value={filter} onChange={(e) => { onFilter?.(e.target.value); setCursor(0) }} placeholder={placeholder} aria-label={placeholder} className="h-8 pl-7 pr-8 text-xs" />
            <Kbd keys="/" className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 opacity-60" />
          </div>
        )}
        {/* Order is filter · hint · action. The hint can carry a control (live/pause) or a notice (a chart selection narrowing the list), so it renders at every width: its own line below md, right-aligned before the action from md. */}
        {hint && <span className="order-last basis-full text-[11px] text-muted-foreground md:order-none md:ml-auto md:basis-auto">{hint}</span>}
        {action && <ActionBar className={cn(!hint && 'sm:ml-auto')}>{action}</ActionBar>}
      </div>}
      {/* One grid for the whole list from md up; each row is a subgrid, so `max-content` tracks size across ALL rows and columns stay aligned. */}
      {state ?? (
        <div ref={listRef} className="op-rows op-scroll-x op-cols border md:grid" style={cols}>
          <div className="op-row hidden items-center gap-x-3 md:col-span-full md:grid md:grid-cols-subgrid">
            {cols_.map((h, i) => {
              const active = h.key && sort?.key === h.key
              return h.key && sortable ? (
                // The ledger is a CSS grid, not an ARIA table, so aria-sort (which needs a columnheader inside a row) is not available; the sort state is spoken as part of the button's name instead.
                <button
                  key={`${h.label}-${i}`}
                  type="button"
                  onClick={() => cycle(h.key!)}
                  title={active ? (sort!.dir === 'asc' ? 'sorted ascending · click for descending' : 'sorted descending · click to clear') : `sort by ${h.label}`}
                  className={cn('op-label group flex min-w-0 items-center gap-1 text-left hover:text-foreground', h.numeric && 'justify-end text-right', active && 'text-foreground')}
                >
                  <span className="min-w-0 truncate">{h.label}</span>
                  {active && <span className="sr-only">, sorted {sort!.dir === 'asc' ? 'ascending' : 'descending'}</span>}
                  <span aria-hidden className={cn('w-2 shrink-0 text-[9px]', !active && 'opacity-0 group-hover:opacity-50')}>{active && sort!.dir === 'desc' ? '▼' : '▲'}</span>
                </button>
              ) : (
                <span key={`${h.label}-${i}`} className={cn('op-label min-w-0 truncate', h.numeric && 'text-right')}>{h.label}</span>
              )
            })}
          </div>
          {sorted.map((r, i) => (
            <div
              key={r.id}
              id={`row-${r.id}`}
              // Roving tabindex: Tab lands on the current row, j/k and arrows move it, Enter opens. Hover does not move it (the pointer is not a selection).
              tabIndex={i === cursor ? 0 : -1}
              role={r.onOpen ? 'button' : undefined}
              aria-current={i === cursor || undefined}
              onFocus={() => setCursor(i)}
              onClick={r.onOpen}
              onKeyDown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) r.onOpen?.() }}
              className={cn('op-row relative grid w-full grid-cols-[1fr_auto] items-center gap-x-3 text-left text-xs outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring md:col-span-full md:grid-cols-subgrid', r.onOpen ? 'cursor-pointer' : 'cursor-default', i === cursor && 'bg-muted', dense ? 'py-0' : 'py-1 md:py-0')}
            >
              {i === cursor && <span aria-hidden className="absolute left-0 top-0 h-full w-0.5 bg-foreground" />}
              <span className="min-w-0 overflow-hidden md:hidden">{r.mobile}</span>
              <span className="md:hidden"><Status state={r.state} label="" /></span>
              {r.cells.map((c, j) => <span key={j} className={cn('hidden min-w-0 truncate md:block', cols_[j]?.numeric && 'text-right')}>{c}</span>)}
            </div>
          ))}
          {sorted.length === 0 && <div className="op-row flex items-center text-xs text-muted-foreground md:col-span-full">{filterable && filter ? <>no match for &quot;{filter}&quot; · <button type="button" className="ml-1 underline underline-offset-4 hover:text-foreground" onClick={() => { onFilter?.(''); inputRef.current?.focus() }}>clear</button></> : 'nothing here yet'}</div>}
          <div className="op-row flex flex-wrap items-center gap-y-1 text-[11px] text-muted-foreground md:col-span-full">
            {/* Paging is the ledger's own footer line: when `page` is set the Pager always renders, and `footer`
                is extra text beside it, never a replacement for it. */}
            {page
              ? <><Pager page={page} className="mr-2" />{footer ? <span className="mr-2">{footer}</span> : null}<span className="hidden lg:inline">· <Kbd keys="[" className="mx-1" /><Kbd keys="]" className="mr-1" /> page · <Kbd keys="j" className="mx-1" /> down · <Kbd keys="k" className="mx-1" /> up · <Kbd keys="⏎" className="mx-1" /> open{filterable && <> · <Kbd keys="/" className="mx-1" /> filter</>}</span></>
              : footer ?? <>{sorted.length} of {total} · <Kbd keys="j" className="mx-1" /> down · <Kbd keys="k" className="mx-1" /> up · <Kbd keys="⏎" className="mx-1" /> open{filterable && <> · <Kbd keys="/" className="mx-1" /> filter</>}</>}
            {sort && (
              <span className="ml-auto flex items-center gap-1">
                sorted by {cols_.find((c) => c.key === sort.key)?.label ?? sort.key} {sort.dir === 'asc' ? '▲' : '▼'} · <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => setSort(null)}>clear</button>
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

/** Fragment-safe id from a section title ("delivery events · ses-eu" → "delivery-events-ses-eu"). */
function slug(t: string) { return t.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') }

// ── Detail ─────────────────────────────────────────────────────────────

export function Detail<T extends string>({ title, meta, mark, status, lede, tabs, tab, onTab, actions, children }: {
  title?: ReactNode
  meta?: ReactNode
  /** Identity mark beside the title; see PageTitle. */
  mark?: ReactNode
  status: ReactNode
  /** The page's answer line (`Lede`). Inside the shell the `status` becomes the header count, so this is what the page itself says first. */
  lede?: ReactNode
  /** Facets of the resource. Omit for a single record that fits one page (an email, a backup run, a scan finding): everything renders in reading order instead. */
  tabs?: readonly T[]
  tab?: T
  onTab?: (t: T) => void
  actions?: ReactNode
  children: ReactNode
}) {
  if (import.meta.env.DEV) {
    if (lede) {
      if (!meta) console.warn('[record recipe] Detail with a lede has no meta; the meta places the record (id · project · environment) so the aside does not have to.')
      if (!status) console.warn('[record recipe] Detail with a lede has no status; every record has a verdict, and "nothing to do" is a verdict.')
    } else if (status && meta && Children.toArray(children).some((c) => isValidElement(c) && c.type === Columns)) {
      // A record page (a verdict, a meta line and a main/aside body) without a lede has no first shape:
      // the eye lands on the tabs. See handoff §7, record page checklist.
      console.warn(`[record recipe] Detail "${String(title ?? '')}" is a record page (status + meta + Columns) with no lede; the lede is the one raised block and the first thing the eye lands on.`)
    }
  }
  useEffect(() => {
    if (!tabs || !onTab) return
    const onKey = (e: globalThis.KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || e.metaKey || e.ctrlKey) return
      const n = Number(e.key)
      if (n >= 1 && n <= tabs.length) onTab(tabs[n - 1])
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [tabs, onTab])
  return (
    <div className="space-y-4">
      {lede ? (
        <div className="flex min-w-0 flex-wrap items-end justify-between gap-x-6 gap-y-2">
          {title && <PageTitle title={title} meta={meta} mark={mark} className="min-w-0 flex-1" />}
          {actions && !tabs && <ActionBar className="pt-5">{actions}</ActionBar>}
        </div>
      ) : title && <PageTitle title={title} meta={meta} mark={mark} />}
      {status}
      {lede}
      {(tabs || (actions && !lede)) && <div className="flex flex-wrap items-center gap-2">
        {/* Tabs scroll horizontally on narrow screens rather than overflowing the page; key badges hide below sm. */}
        {tabs && <ScrollRow role="tablist" className="[&>div]:border [&>div]:text-xs">
          {tabs.map((t, i) => (
            <button key={t} role="tab" aria-selected={tab === t} onClick={() => onTab?.(t)} ref={(el) => { if (el && tab === t) el.scrollIntoView({ block: 'nearest', inline: 'nearest' }) }} className={cn('inline-flex h-8 shrink-0 items-center gap-1 whitespace-nowrap px-3 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring', i > 0 && 'border-l', tab === t ? 'bg-foreground text-background' : 'hover:bg-muted')}>
              {t} <Kbd keys={String(i + 1)} className="ml-1 hidden opacity-60 sm:inline-flex" />
            </button>
          ))}
        </ScrollRow>}
        {actions && <ActionBar className="sm:ml-auto">{actions}</ActionBar>}
      </div>}
      {children}
    </div>
  )
}

/** Segmented toggle used for compare / range choices inside a Detail. */
export function Segmented<T extends string>({ options, value, onChange, className }: { options: readonly (readonly [T, string])[]; value: T; onChange: (v: T) => void; className?: string }) {
  return (
    <div className={cn('op-scroll-x flex min-w-0 max-w-full border text-xs', className)}>
      {options.map(([v, l], i) => (
        <button key={v} type="button" aria-pressed={value === v} onClick={() => onChange(v)} className={cn('h-8 shrink-0 whitespace-nowrap px-3', i > 0 && 'border-l', value === v ? 'bg-muted' : 'hover:bg-muted')}>{l}</button>
      ))}
    </div>
  )
}

// ── Settings ───────────────────────────────────────────────────────────

export function Settings({ title, meta, status, sections, onSave, dirty, danger }: {
  title?: ReactNode
  meta?: ReactNode
  status: ReactNode
  sections: { title: string; body: ReactNode }[]
  onSave: () => void
  dirty: boolean
  /** Contents of the danger zone. Use <EchoDialog> for the action. */
  danger: ReactNode
}) {
  const saveBtn = useRef<HTMLButtonElement>(null)
  const [pressed, setPressed] = useState(false)
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
        e.preventDefault()
        setPressed(true)
        window.setTimeout(() => setPressed(false), 150)
        saveBtn.current?.click() // ⌘S clicks the real button, so it shows pressed and disabled states honestly
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  return (
    <div className="@container space-y-4">
      {title && <PageTitle title={title} meta={meta} />}
      {status}
      <div className="grid gap-6 @3xl:grid-cols-[180px_minmax(0,1fr)]">
        <nav className="hidden text-xs @3xl:block">
          {sections.map((s) => <a key={s.title} href={`#s-${slug(s.title)}`} className="block py-1 text-muted-foreground hover:text-foreground">{s.title}</a>)}
          <a href="#s-danger" className="block py-1 text-destructive">danger zone</a>
        </nav>
        <div className="space-y-6">
          {sections.map((s) => (
            <section key={s.title} id={`s-${slug(s.title)}`} className="border">
              <div className="border-b px-4 py-2"><h2 className="op-h3">{s.title}</h2></div>
              <div className="@container space-y-4 p-4">{s.body}</div>
            </section>
          ))}
          <section id="s-danger" className="border border-destructive">
            <div className="border-b border-destructive px-4 py-2"><h2 className="op-h3 text-destructive">danger zone</h2></div>
            <div className="p-4">{danger}</div>
          </section>
        </div>
      </div>
      <div className={cn('op-sticky-bottom flex items-center gap-3 border-t bg-background px-4 py-2 text-xs @3xl:-mx-6 @3xl:px-6', !dirty && 'text-muted-foreground')}>
        <span>{dirty ? 'unsaved changes' : 'no changes'}</span>
        <Button ref={saveBtn} size="sm" disabled={!dirty} onClick={onSave} className={cn('op-primary ml-auto h-8 text-xs', pressed && 'op-pressed')}>
          save <Kbd keys={['⌘', 'S']} className="ml-1 opacity-70" />
        </Button>
      </div>
    </div>
  )
}

/** Label · control · help. One row when the section is wide enough (container query), stacked otherwise. */
export function Field({ label, children, help }: { label: string; children: ReactNode; help?: string }) {
  return (
    <label className="grid gap-1 text-xs @md:grid-cols-[160px_minmax(0,1fr)] @md:items-center @md:gap-4">
      <span className="op-label">{label}</span>
      <span className="space-y-1">
        {children}
        {help && <span className="block text-[11px] text-muted-foreground">{help}</span>}
      </span>
    </label>
  )
}

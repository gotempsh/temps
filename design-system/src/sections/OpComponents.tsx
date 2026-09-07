// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { Bot, Box, Container, Cpu, FileText, GitBranch, Link as LinkIcon, Loader2, Monitor, RefreshCw, Rocket, RotateCcw, Search, Share2, Smartphone, Tablet, Trash2 } from 'lucide-react'
import { DocPage, Rule } from '@/components/op-doc'
import {
  Breakdown, Sparkline, StatusStrip, ScoreRing, CalendarHeatmap, Funnel, Flow, Waterfall, StackTrace, LogLines, Stages, Histogram, Live, ProjectMark,
  type Span, type Frame, type LogLine as OpLogLine, type Pct, type StatusBucket, GeoMap, Callout } from '@/components/op'
import { Button } from '@/components/ui/button'
import {
  ChartFooter, Detail, EchoDialog, Field, Kbd, KbdPair, Ledger, Metric, MetricGrid, Num, PageState, Phrase, RangePicker,
  PageTitle, Picker, Section, Segmented, Settings, ShellSlotsProvider, Status, StatusLine, TimeChart, worst,
  type LedgerRow, type State,
} from '@/components/op'
// `Inspector` is not re-exported from the package barrel yet (that one line in
// `web/packages/ds/src/index.ts` belongs to whoever owns the package this pass),
// so it is reached by path. Swap this for `@/components/op` the moment the
// barrel carries it — nothing else about the block changes.
import { Inspector, type InspectorAnchor } from '../../../web/packages/ds/src/inspector'
import { CopyAction } from '../../../web/packages/ds/src/copy'
import { Article, CodeBlock, ImageFigure } from '@/components/op'
import { BRANCHES } from './ConsoleV1'
// The five blocks files are the live half of docs/{forms,notifications,content,
// data-viz,motion,icons}.md. The guide mounts the same components section by
// section; the gallery mounts them all, so no primitive is documented in only
// one of the two places.
import { ContentBlocks } from './blocks/ContentBlocks'
import { DataVizBlocks } from './blocks/DataVizBlocks'
// The second wave of charts and the generative-UI blocks. Their `*_TOC`
// exports are the source of truth for the ids and the labels: the rail below
// spreads them rather than restating them, so the rail and the DOM cannot drift.
import { DATAVIZ2_TOC, DataVizBlocks2 } from './blocks/DataVizBlocks2'
import { FormBlocks } from './blocks/FormBlocks'
import { GENUI_TOC, GenUiBlocks } from './blocks/GenUiBlocks'
import { NotificationBlocks } from './blocks/NotificationBlocks'
import { TokenBlocks } from './blocks/TokenBlocks'


// ── viz demo data (shapes follow docs/console-inventory.md) ────────────
const flag = (cc: string) => <span className="text-[13px] leading-none">{String.fromCodePoint(...[...cc.toUpperCase()].map((c) => 0x1f1e6 + c.charCodeAt(0) - 65))}</span>
const VZ_CHANNELS = [{ label: 'direct', count: 4810, icon: <LinkIcon /> }, { label: 'organic search', count: 3120, icon: <Search /> }, { label: 'social', count: 1240, icon: <Share2 /> }, { label: 'ai agents', count: 498, icon: <Bot />, state: 'sampled' as const }]
const VZ_GEO = [{ geo: 'United States of America', label: 'United States', value: 'TTFB 1.21s · needs work', state: 'warn' as const, note: '4,312 samples' }, { geo: 'Germany', label: 'Germany', value: 'TTFB 210ms · good', state: 'ok' as const, note: '1,820 samples' }, { geo: 'Spain', label: 'Spain', value: 'TTFB 240ms · good', state: 'ok' as const }, { geo: 'United Kingdom', label: 'United Kingdom', value: 'TTFB 410ms · good', state: 'ok' as const }, { geo: 'Brazil', label: 'Brazil', value: 'TTFB 1.42s · needs work', state: 'warn' as const }, { geo: 'India', label: 'India', value: 'TTFB 1.65s · needs work', state: 'warn' as const }, { geo: 'Nigeria', label: 'Nigeria', value: 'TTFB 2.40s · poor', state: 'error' as const }]
const VZ_DEVICES = [{ label: 'desktop', count: 7920, icon: <Monitor /> }, { label: 'mobile', count: 3890, icon: <Smartphone /> }, { label: 'tablet', count: 520, icon: <Tablet /> }]
const VZ_LOCATIONS = [
  { label: 'United States', icon: flag('us'), count: 4312, children: [{ label: 'California', count: 1610, children: [{ label: 'San Francisco', count: 720 }, { label: 'Los Angeles', count: 480 }] }, { label: 'New York', count: 980 }, { label: 'Texas', count: 640 }] },
  { label: 'Germany', icon: flag('de'), count: 1820, children: [{ label: 'Berlin', count: 760 }, { label: 'Bavaria', count: 520 }] },
  { label: 'United Kingdom', icon: flag('gb'), count: 1404 }, { label: 'Spain', icon: flag('es'), count: 980 }, { label: 'France', icon: flag('fr'), count: 812 }, { label: 'Portugal', icon: flag('pt'), count: 611 },
]
const VZ_STRIP: StatusBucket[] = Array.from({ length: 48 }, (_, i) => ({ start: `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`, state: i === 41 ? 'error' : i === 40 || i === 42 ? 'warn' : 'ok', checks: 60, down: i === 41 ? 60 : 0, p50_ms: i === 41 ? undefined : 90 + ((i * 7) % 40), p95_ms: i === 41 ? undefined : 300 + ((i * 11) % 90) }))
/**
 * 12 weeks of deploys, each day naming what shipped. `ids` is what makes the
 * readout worth reading — "3 deploys" is a number, "dep_2ha · dep_2hb · dep_2hc"
 * is something you can open. Moved here with the readout demo it feeds.
 */
const VZ_DAYS = Array.from({ length: 12 * 7 }, (_, i) => {
  const count = i % 7 === 5 || i % 7 === 6 ? (i % 4 === 0 ? 1 : 0) : Math.floor(Math.abs(Math.sin(i / 2.7)) * 9)
  return {
    date: `2026-0${6 + Math.floor(i / 30)}-${String(1 + (i % 30)).padStart(2, '0')}`,
    count,
    ids: Array.from({ length: count }, (_, k) => `dep_${(80 + i).toString(36)}${'abcdefghi'[k]}`),
  }
})
const VZ_SPANS: Span[] = [
  { id: 'r', name: 'POST /checkout', service: 'api-gateway', start_ms: 0, duration_ms: 812, state: 'error', children: [
    { id: 'a', name: 'auth.verify', service: 'api-gateway', start_ms: 2, duration_ms: 14 },
    { id: 'c', name: 'cart.load', service: 'api-gateway', start_ms: 18, duration_ms: 61, children: [{ id: 'c1', name: 'SELECT carts', service: 'postgres', start_ms: 20, duration_ms: 48 }] },
    { id: 's', name: 'stripe.charge', service: 'stripe', start_ms: 84, duration_ms: 690, state: 'error', children: [{ id: 's1', name: 'POST /v1/charges', service: 'stripe', start_ms: 86, duration_ms: 684, state: 'error' }] },
    { id: 'e', name: 'email.enqueue', service: 'api-gateway', start_ms: 780, duration_ms: 6 },
  ] },
]
const VZ_FRAMES: Frame[] = [
  { fn: 'chargeCard', file: 'src/checkout/charge.ts', line: 48, col: 11, inApp: true, original: 'charge.ts', context: [{ line: 46, code: "  const intent = await stripe.charges.create(body)" }, { line: 47, code: '  if (!intent.paid) {' }, { line: 48, code: "    throw new PaymentError(intent.failure_message ?? 'declined')" }, { line: 49, code: '  }' }, { line: 50, code: '  return intent' }] },
  { fn: 'handler', file: 'src/routes/checkout.ts', line: 21, col: 5, inApp: true, context: [{ line: 20, code: '  const cart = await loadCart(req.user.id)' }, { line: 21, code: '  const charge = await chargeCard(cart, req.body.card)' }, { line: 22, code: '  await enqueueReceipt(charge)' }] },
  { fn: 'dispatch', file: 'node_modules/hono/dist/hono-base.js', line: 187 },
  { fn: 'processTicksAndRejections', file: 'node:internal/process/task_queues', line: 95 },
]
const VZ_LOG: OpLogLine[] = [
  { t: '20:31:02', level: 'info', source: 'api-gateway', msg: 'listening on :8080' },
  { t: '20:31:04', level: 'debug', source: 'api-gateway', msg: 'pool: 8 connections warm' },
  { t: '20:31:19', level: 'warn', source: 'stripe', msg: 'retrying POST /v1/charges (attempt 2) after 429' },
  { t: '20:31:20', level: 'error', source: 'api-gateway', msg: 'PaymentError: card_declined · order ord_48211 · trace 7f1e…' },
  { t: '20:31:20', level: 'info', source: 'api-gateway', msg: 'POST /checkout 402 812ms' },
  { t: '20:31:31', level: 'debug', source: 'api-gateway', msg: 'gc: 12ms' },
]
const VZ_STAGES = [
  { phase: 'build', name: 'clone', state: 'ok' as const, duration: '3s', result: 'main@9bc61c0 · 1,204 files' },
  { phase: 'build', name: 'install', state: 'ok' as const, duration: '41s', result: '412 packages', lines: VZ_LOG.slice(0, 2) },
  { phase: 'build', name: 'build', state: 'idle' as const, duration: '12s', result: '312 modules so far', lines: [{ t: '20:31:50', level: 'info' as const, source: 'bun', msg: 'bun build ./src/index.ts --target node' }, { t: '20:31:53', level: 'info' as const, source: 'bun', msg: '  312 modules · 1.9 MB' }] },
  { phase: 'release', name: 'start containers', state: 'idle' as const },
  { phase: 'release', name: 'switch traffic', state: 'idle' as const },
]
const VZ_HIST = [5, 10, 25, 50, 100, 250, 500, 1000, 2500].map((le, i) => ({ le, count: [40, 180, 620, 1450, 1720, 980, 310, 90, 22][i] }))

/* ────────────────────────────────────────────────────────────────────────
   /op-components — the operator component library, one block per
   component: what it is for, its rules, every state, and the props that
   matter. This page is the reference the handoff document points at.
   ──────────────────────────────────────────────────────────────────────── */

const TOC = [
  ['status', 'Status · StatusLine'],
  ['num', 'Num · Metric'],
  ['page-state', 'PageState'],
  ['button-busy', 'Button · busy'],
  ['copy-action', 'CopyAction'],
  ['kbd', 'Kbd'],
  ['echo', 'EchoDialog'],
  ['chart', 'TimeChart · RangePicker'],
  ['ledger', 'Ledger'],
  ['inspector', 'Inspector'],
  ['detail', 'Detail · PageTitle'],
  ['picker', 'Picker'],
  ['settings', 'Settings'],
  ['mark', 'ProjectMark'],
  ['breakdown', 'Breakdown · Sparkline · Funnel · Flow'],
  ['callout', 'Callout'],
  ['strip', 'StatusStrip · ScoreRing · Heatmap · Live'],
  ['trace', 'Waterfall · StackTrace'],
  ['logs', 'LogLines · Stages · Histogram'],
  ['form-field', 'Field · hint, error, optional'],
  ['form-validation', 'FormErrors · validation and save'],
  ['form-datetime', 'Dates and times'],
  ['form-range-schedule', 'Ranges, schedules, durations'],
  ['form-disabled', 'Disabled and unavailable'],
  ['notify-table', 'Which surface says it'],
  ['notify-toast', 'Toasts'],
  ['notify-attention', 'The bell · AttentionHost'],
  ['viz-choice', 'Pick the chart from the question'],
  ['viz-series', 'TimeChart · legend and table view'],
  ['viz-legend', 'Series without a second hue'],
  ['viz-a11y', 'Charts read without the picture'],
  // Spread, never retyped: DataVizBlocks2 and GenUiBlocks own their ids.
  ...DATAVIZ2_TOC,
  ...GENUI_TOC,
  ['content-error', 'Error messages'],
  ['content-time', 'Time and the id beside it'],
  ['content-fmt', 'fmt · the formatters'],
  ['prose', '.op-prose · a body of prose'],
  ['article', 'Article · CodeBlock · ImageFigure'],
  ['tokens-table', 'Tokens · tokens.json'],
  ['motion-tiers', 'Motion utilities'],
  ['icons-vocabulary', 'Icons · the vocabulary'],
] as const

function Block({ id, title, rule, api, children }: { id: string; title: string; rule: ReactNode; api: string; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-16 border-t pt-8">
      <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
        <div className="min-w-0">
          <h2 className="op-h2">{title}</h2>
          {/* The prose face, but not a prose body: `.op-raw` keeps the long-form rules off a doc block. */}
          <div className="op-prose op-raw mt-2 space-y-2 text-sm text-muted-foreground">{rule}</div>
          <pre tabIndex={0} className="op-inset mt-4 overflow-auto border p-3 font-mono text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">{api}</pre>
        </div>
        <div className="min-w-0 space-y-4">{children}</div>
      </div>
    </section>
  )
}

function Demo({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col">
      <p className="op-label mb-2">{label}</p>
      <div className="flex-1 px-4 sm:px-6">{children}</div>
    </div>
  )
}

/**
 * The six states, in the order the legend reads them. `running` sits beside
 * `warn` because that is where `STATE_RANK` puts it (error 0, warn 1,
 * running 2, sampled 3, ok 4, idle 5): work in progress outranks anything
 * healthy in a "needs attention first" list, but it is not a verdict.
 */
// STATE_RANK order: worst first, the way a sorted ledger reads.
const STATES: State[] = ['error', 'warn', 'running', 'sampled', 'ok', 'idle']
const SERIES = Array.from({ length: 24 }, (_, i) => ({ t: `${String(i).padStart(2, '0')}:00`, req: Math.round(400 + 600 * Math.max(0, Math.sin(((i - 6) / 24) * Math.PI * 2)) + (i > 15 ? 250 : 0)) }))

/** A stand-in shell header: breadcrumb slot on the left, attention slot on the right, a StatusLine inside. */
function HeaderSlotDemo() {
  const [crumb, setCrumb] = useState<HTMLElement | null>(null)
  const [attention, setAttention] = useState<HTMLElement | null>(null)
  return (
    <div className="border">
      <div className="flex h-11 items-center gap-2 border-b px-3 text-xs">
        <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1.5 truncate text-muted-foreground"><span>platform</span><span aria-hidden className="text-[var(--op-rule-soft)]">/</span><span ref={setCrumb} className="flex items-center gap-1.5 [&>span:first-child]:hidden" /></nav>
        <div className="ml-auto flex items-center gap-2"><ul ref={setAttention} className="contents" /><span className="hidden h-7 items-center border px-2 text-muted-foreground sm:flex">find</span></div>
      </div>
      <div className="px-3 pb-3">
        <ShellSlotsProvider value={{ crumb, attention }}>
          <PageTitle title="Projects" meta="6 projects · 2 need attention" />
          <StatusLine state={worst(['error', 'warn', 'ok'])} more={{ label: '+1 warning', items: [{ state: 'warn', children: <><Phrase>api-gateway</Phrase> error rate 0.61% since dep_91a.</> }] }}>
            <Phrase>billing-worker</Phrase> is failing health checks.
          </StatusLine>
        </ShellSlotsProvider>
        <p className="mt-2 text-xs text-muted-foreground">The page below the title starts here; nothing between the header and the title but air.</p>
      </div>
    </div>
  )
}

/** 1,284 rows server-side, 20 at a time. The footer carries the range, prev/next, page size; filter resets to page 1. */
function PagedLedgerDemo() {
  const [page, setPage] = useState(1)
  const [size, setSize] = useState(20)
  const [q, setQ] = useState('')
  const total = 1284
  const all = Array.from({ length: total }, (_, i) => ({ id: `em_${(0x9000 + i).toString(16)}`, to: `${['sam', 'ana', 'lee', 'kim'][i % 4]}${i}@example.com`, status: i % 23 === 11 ? 'bounced' : 'delivered', at: `${i + 1}m ago` }))
  const needle = q.trim().toLowerCase()
  const filtered = all.filter((r) => !needle || r.to.toLowerCase().includes(needle) || r.id.toLowerCase().includes(needle))
  const rows: LedgerRow[] = filtered.slice((page - 1) * size, page * size).map((r) => ({
    id: r.id, state: r.status === 'bounced' ? 'error' : 'ok',
    cells: [<span key="i" className="font-mono text-muted-foreground">{r.id}</span>, <span key="t" className="truncate font-mono">{r.to}</span>, <Status key="s" state={r.status === 'bounced' ? 'error' : 'ok'} label={r.status} />, <span key="a" className="text-muted-foreground">{r.at}</span>],
    mobile: <><span className="font-mono">{r.to}</span><span className="block text-muted-foreground">{r.status} · {r.at}</span></>,
  }))
  return (
    <Ledger status={null} columns={['id', 'to', 'status', 'when']} grid="minmax(70px,max-content) minmax(8rem,1.5fr) minmax(90px,max-content) minmax(60px,max-content)"
      rows={rows} total={total} filter={q} onFilter={(v) => { setQ(v); setPage(1) }} placeholder="filter by recipient or id"
      dense={false} page={{ page, pageSize: size, total: filtered.length, onPage: setPage, onPageSize: (n) => { setSize(n); setPage(1) } }} />
  )
}

/**
 * `Inspector` inspects a ledger row beside the list instead of navigating away
 * from it. Drawn here against the documented shape while the primitive lands in
 * `@temps-sdk/ds`: ~520px pushing the main column on `xl`, an overlay sheet
 * with a scrim below `xl`, full-screen below `md`; an ink rule on its left
 * edge; a header of glyph + word, mono title, meta and three actions; stacked
 * `Section`s under a small in-panel toc row.
 *
 * The point of the primitive is that the list keeps the cursor: `j`/`k` keep
 * moving the ledger's cursor and the panel follows, so a reader walks a list
 * reading each row without ever losing their place.
 */
const INSPECT: { id: string; state: State; word: string; title: string; meta: string }[] = [
  { id: 'log_03x9w6', state: 'error', word: 'error', title: 'health check GET /healthz timed out after 30s', meta: 'billing-worker · production · 31m ago' },
  { id: 'log_03x951', state: 'warn', word: 'warn', title: 'container restarted after 3 health check failures', meta: 'billing-worker · production · 44m ago' },
  { id: 'log_03x8dw', state: 'running', word: 'building', title: 'next build · 31 of 48 routes', meta: 'acme-storefront · staging · now' },
]

function InspectorDemo() {
  // Open on a desktop, closed on a phone — deliberately, and this is the
  // component's own rule rather than a demo trick: from `xl` the panel is
  // `sticky` and therefore in flow (the ledger is pushed, never covered), so
  // it can be shown standing open. Below `xl` it is a sheet over a scrim and
  // below `md` it is the whole screen, and a gallery page that opened a
  // full-screen sheet over itself on load would bury the seventeen blocks
  // under it. On a narrow screen the reader opens a row to see it.
  const [open, setOpen] = useState<string | null>(() =>
    typeof window !== 'undefined' && window.matchMedia('(min-width: 1280px)').matches ? INSPECT[0].id : null)
  const rowRefs = useRef<Record<string, HTMLElement | null>>({})
  const row = INSPECT.find((r) => r.id === open) ?? INSPECT[0]
  const [log, setLog] = useState<string | null>(null)
  const rows: LedgerRow[] = INSPECT.map((r) => ({
    id: r.id,
    state: r.state,
    cells: [
      <span key="m" className="min-w-0 truncate">{r.title}</span>,
      <span key="i" className="font-mono text-[11px] text-muted-foreground">{r.id}</span>,
    ],
    mobile: <span className="min-w-0"><span className="block truncate">{r.title}</span><span className="block truncate font-mono text-[11px] text-muted-foreground">{r.id}</span></span>,
    onOpen: () => setOpen(r.id),
  }))
  const ANCHORS: InspectorAnchor[] = [
    { id: 'insp-fields', label: 'fields' },
    { id: 'insp-trace', label: 'trace' },
    { id: 'insp-request', label: 'request' },
    { id: 'insp-context', label: 'context' },
  ]
  return (
    <div ref={(el) => { if (el) for (const r of INSPECT) rowRefs.current[r.id] = el.querySelector<HTMLElement>(`[data-row="${r.id}"]`) }}
      className="flex min-w-0 items-start gap-4">
      <div className="min-w-0 flex-1">
        <Ledger status={null} dense columns={['line', 'id']} grid="minmax(0,1fr) minmax(96px,max-content)" rows={rows} total={INSPECT.length} hint="needs attention first" />
        <p className="mt-2 font-mono text-[11px] text-muted-foreground">
          {log ?? 'open a row · j k keep moving this list and the panel follows · esc closes and comes back here'}
        </p>
      </div>
      <Inspector
        open={open !== null}
        label="log line inspector"
        state={row.state}
        word={row.word}
        title={row.id}
        meta={row.meta}
        anchors={ANCHORS}
        onOpen={() => setLog(`open ${row.id} · the full record page`)}
        copyLink={`${window.location.origin}/op-components?inspect=${row.id}`}
        onCopied={() => setLog(`link to ${row.id} on the clipboard · the control said so, not a toast`)}
        onClose={() => setOpen(null)}
        returnFocus={() => rowRefs.current[row.id]}
      >
        <div id="insp-fields">
          <Section title="Fields" meta="4 fields">
            <dl className="grid grid-cols-[minmax(72px,max-content)_minmax(0,1fr)] gap-x-3 gap-y-1 text-[11px]">
              {[['method', 'GET'], ['route', '/healthz'], ['status', '504'], ['duration', '30,000 ms']].map(([k, v]) => (
                <div key={k} className="contents"><dt className="text-muted-foreground">{k}</dt><dd className="min-w-0 truncate font-mono">{v}</dd></div>
              ))}
            </dl>
          </Section>
        </div>
        <div id="insp-trace">
          <Section title="Trace" meta="d4e5f6a7 · 6 spans">
            <p className="text-[11px] text-muted-foreground">The trace this line belongs to, read here rather than in a page the reader has to come back from.</p>
          </Section>
        </div>
        <div id="insp-request">
          <Section title="Request" meta="req_77c210">
            <p className="text-[11px] text-muted-foreground">Every line the same request wrote, in order.</p>
          </Section>
        </div>
        <div id="insp-context">
          <Section title="Context" meta="±20 lines">
            <p className="text-[11px] text-muted-foreground">The lines either side, from the same container: what a log record is actually read for.</p>
          </Section>
        </div>
      </Inspector>
    </div>
  )
}

export function OpComponentsPage() {
  const [retrying, setRetrying] = useState(false)
  const [busy, setBusy] = useState(false)
  const [hot, setHot] = useState<string | null>(null)
  const [range, setRange] = useState('24h')
  const [gate, setGate] = useState<string | null>(null)
  const [q, setQ] = useState('')
  const [tab, setTab] = useState<'overview' | 'deploys' | 'logs'>('overview')
  const [seg, setSeg] = useState<'today' | 'yesterday'>('today')
  const [form, setForm] = useState({ branch: 'main' })
  const [saved, setSaved] = useState(form)
  const [branch, setBranch] = useState('main')
  const [env, setEnv] = useState<string | null>(null)
  const [space, setSpace] = useState<string>('worktree')
  const [pickState, setPickState] = useState<'loading' | 'error' | 'ok'>('error')
  const [span, setSpan] = useState<string>('s1')
  const [pct, setPct] = useState<Pct>('p95')
  const [livePaused, setLivePaused] = useState(false)
  const [log, setLog] = useState<string[]>([])

  const rows: LedgerRow[] = ([
    // `icon` is the row's kind (app · static · worker), muted ink in a fixed 16px slot at the
    // head of the first cell: the list mixes kinds, so brand §6 owes it the slot. The glyph keeps its own.
    { id: 'api-gateway', state: 'warn' as State, icon: <Box aria-hidden />, sort: { name: 'api-gateway', visitors: 30800, err: 0.61 }, mobile: <span>api-gateway</span>, cells: [<span className="font-medium">api-gateway</span>, <Status state="warn" label="error rate above 0.5%" />, <Num value={30800} />, <Num value="0.61" unit="%" className="text-destructive" />], onOpen: () => setLog((l) => ['open api-gateway', ...l]) },
    { id: 'docs', state: 'ok' as State, icon: <FileText aria-hidden />, sort: { name: 'docs', visitors: 2210, err: 0 }, mobile: <span>docs</span>, cells: [<span className="font-medium">docs</span>, <Status state="ok" label="production" />, <Num value={2210} />, <Num value="0.00" unit="%" />], onOpen: () => setLog((l) => ['open docs', ...l]) },
    { id: 'acme-web', state: 'idle' as State, icon: <Cpu aria-hidden />, sort: { name: 'acme-web', visitors: null, err: null }, mobile: <span>acme-web</span>, cells: [<span className="font-medium">acme-web</span>, <Status state="idle" label="not deployed" />, <Num value={null} />, <Num value={null} />] },
  ] as LedgerRow[]).filter((r) => r.id.includes(q))

  return (
    <DocPage
      eyebrow="operator components · @temps-sdk/ds"
      intro={<>
        The components the three page templates are built from. Each block: the rule, the props that matter, every state. <Link to="/v1" className="underline underline-offset-4">/v1</Link> is these assembled into a console; the handoff document is <span className="font-mono">docs/design-system-handoff.md</span>. The last five groups — forms, notifications, charts, content and the token, motion and icon tables — are the same blocks <Link to="/guide#forms" className="underline underline-offset-4">/guide</Link> mounts beside their documents.
      </>}
      toc={TOC}
    >
          <Block id="status" title="Status · StatusLine · Phrase" api={`<Status state="warn" label="error rate above 0.5%" />
<StatusLine state={worst(states)} more={{ label: '+1 warning', items: [
  { state: 'warn', children: <><Phrase onClick={open}>api-gateway</Phrase> error rate 0.61% since dep_91a.</> },
] }}>
  <Phrase onClick={open}>billing-worker</Phrase> is failing health checks.
</StatusLine>`}
            rule={<><p>Six states. Colour only ever appears through them, and always with a glyph and a word. <code>sampled</code> exists because pricing promises the console says when telemetry is head-sampled. <code>running</code> is the odd one out: it takes <em>no</em> hue — plain ink, <code>◉</code>, and a slow opacity-only pulse (<code>.op-pulse</code>) that is the only motion in the system — because work in progress is not a verdict. Its word comes from the operation, never from the state: <em>building</em>, <em>restoring</em>, <em>scanning</em>.</p><p>The status line is the page's verdict: one glyph (the worst state on the page), one sentence under 60 characters, at most one link. Inside the console shell it takes no line of the page: it renders into the header as a glyph and a count, and the sentences open on demand. Outside a shell it renders inline, with further problems behind <code>more</code> on the right. Facts and counts never appear in a verdict.</p></>}>
            <Demo label="the six states">
              <div className="flex flex-wrap gap-6 text-sm">{STATES.map((s) => <Status key={s} state={s} label={s} />)}</div>
            </Demo>
            <Demo label="running · the word is the operation, not the state">
              <div className="flex flex-wrap gap-6 text-sm">
                <Status state="running" label="building" />
                <Status state="running" label="restoring" />
                <Status state="running" label="scanning" />
              </div>
            </Demo>
            <Demo label="inside the shell · header attention indicator, click it">
              <HeaderSlotDemo />
            </Demo>
            <Demo label="outside a shell · the inline form">
              <StatusLine sticky={false} state={worst(['error', 'warn', 'ok'])} more={{ label: '+1 warning', items: [{ state: 'warn', children: <><Phrase>api-gateway</Phrase> error rate 0.61% since dep_91a.</> }] }}>
                <Phrase>billing-worker</Phrase> is failing health checks.
              </StatusLine>
            </Demo>
            <Demo label="status line · everything fine">
              <StatusLine sticky={false} state="ok">All 6 projects healthy.</StatusLine>
            </Demo>
            <Demo label="status line · sampled (Starter past 10 GB)">
              <StatusLine sticky={false} state="sampled">Telemetry is sampled 1 in 4 since 14:00. <Phrase>Why</Phrase></StatusLine>
            </Demo>
            <Demo label="wrong · an inventory, not a verdict">
              <p className="op-status border-b pb-3 text-sm leading-6 opacity-60"><span className="text-warning">◐</span> <a href="#">6 projects</a> · <span className="text-destructive">×</span> <a href="#">billing-worker failing health checks</a> · <span className="text-warning">◐</span> <a href="#">api-gateway error rate 0.61%</a> · <a href="#">4 deploys today</a> · <a href="#">cert expires in 6d</a></p>
            </Demo>
            <Demo label="still wrong · two clauses, two links, a tail that truncates">
              <p className="op-status flex items-baseline gap-2 border-b pb-3 text-sm leading-6 opacity-60"><span className="text-destructive">×</span><span className="min-w-0 truncate"><a href="#">billing-worker</a> is failing health checks. <a href="#">api-gateway</a> error rate 0.61% since dep_91a. <span>4 healthy · cert for cdn.acme.sh expires in 6d</span></span></p>
            </Demo>
          </Block>

          <Block id="num" title="Num · Metric · MetricGrid" api={`<Num value={30800} />          30,800
<Num value="0.61" unit="%" />  0.61%
<Num value={null} />           –
<MetricGrid cols={4}>
  <Metric label="error rate" value="0.61" unit="%" delta="+0.2pt" baseline="since dep_91a" state="warn" />
</MetricGrid>`}
            rule={<><p>Numbers are mono and tabular. The unit follows the value in muted. Nothing is an en dash, zero is 0.</p><p><code>baseline</code> on Metric is required. A delta without its comparison is a number pretending to mean something.</p></>}>
            <Demo label="metric grid">
              <MetricGrid cols={4}>
                <Metric label="requests · 24h" value="30.8k" delta="+9%" baseline="since dep_91a" />
                <Metric label="error rate" value="0.61" unit="%" delta="+0.2pt" baseline="since dep_91a" state="warn" />
                <Metric label="p95 latency" value={184} unit="ms" delta="−9ms" baseline="vs dep_90e" />
                <Metric label="uptime · 90d" value="99.94" unit="%" baseline="2 incidents · 90d window" />
              </MetricGrid>
            </Demo>
            <Demo label="values">
              <p className="flex flex-wrap gap-6 text-sm"><Num value={30800} /> <Num value="4.2" unit=" GB" /> <Num value={0} /> <Num value={null} /> <Num value="0.61" unit="%" className="text-destructive" /></p>
            </Demo>
          </Block>

          <Block id="page-state" title="PageState" api={`<PageState state="loading" rows={4} />
<PageState state="empty" title reason next? />
<PageState state="unconfigured" title missing example settingsHref settingsLabel />
<PageState state="error" title message resource onRetry retrying? />`}
            rule={<><p>One component for every non-happy state. Replaces the three empty-state implementations, the spinners used as page state, and the ad-hoc error banners.</p><p><code>unconfigured</code> must show an example of what the surface will show. A feature you cannot picture is a feature you will not set up. Nothing ever renders as blank.</p></>}>
            <Demo label="loading"><PageState state="loading" rows={3} /></Demo>
            <Demo label="empty"><PageState state="empty" title="No deploys yet today" reason="The last deploy was dep_88c, 3 days ago. Pushes to main deploy automatically." next={<Button size="sm" className="op-primary h-8 text-xs"><Rocket /> deploy now <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button>} /></Demo>
            <Demo label="unconfigured">
              <PageState state="unconfigured" title="No tracking snippet on any project" missing="the one-line script tag in your app's <head>. No cookies, no consent banner." settingsHref="/settings" settingsLabel="get the snippet"
                example={<div className="space-y-2 font-mono text-[11px]"><p>● acme-storefront · 12,400 visitors · 24h · +8% vs last week</p><div className="flex h-12 items-end gap-px">{Array.from({ length: 36 }, (_, i) => <span key={i} className="flex-1 bg-foreground/60" style={{ height: `${20 + Math.abs(Math.sin(i / 5)) * 70}%` }} />)}</div></div>} />
            </Demo>
            <Demo label="error"><PageState state="error" title="Error store unreachable" message="connection refused: clickhouse://127.0.0.1:9000 (timeout 3s)" resource="clickhouse · events-ch" retrying={retrying} onRetry={() => { setRetrying(true); window.setTimeout(() => setRetrying(false), 900) }} /></Demo>
          </Block>

          <Block id="button-busy" title="Button · busy" api={`<Button busy={saving} busyLabel={<><Loader2 /> saving…</>}>
  save
</Button>

// busy is NOT disabled: the colour, the focus and the
// width all stay. A minimum 400ms holds off the flicker.`}
            rule={<>
              <p>
                A button whose work is still running spins <em>its own icon</em> and changes <em>its own label</em>.
                This is the one sanctioned spinner in the system — a reload spins because the thing it stands for
                goes round. A state glyph pulses instead (<code>running</code> ◉), because a state is not an action.
              </p>
              <Rule state="ok">Busy, never <code>disabled</code>. Disabling greys the control out and drops focus, so a keyboard reader who just pressed ⌘S is thrown to the top of the document at the moment they are waiting to hear what happened.</Rule>
              <Rule state="ok">The width is locked to the idle width, so “save” becoming “saving…” does not move the row it sits in.</Rule>
              <Rule state="ok">A minimum busy time of 400ms: under that, a fast answer is a flicker the eye cannot read.</Rule>
              <Rule state="ok">Under <code>prefers-reduced-motion</code> the icon holds still and the label alone says it — which is why <code>busyLabel</code> is not really optional.</Rule>
              <Rule state="error">A spinner anywhere else: as page state (that is <code>PageState</code>), beside a row, or in a header.</Rule>
            </>}>
            <Demo label="idle · busy · the width does not move">
              <div className="flex flex-wrap items-center gap-3">
                <Button size="sm" className="op-primary h-8 text-xs" busy={busy} busyLabel={<><Loader2 aria-hidden /> saving…</>}
                  onClick={() => { setBusy(true); window.setTimeout(() => setBusy(false), 1600) }}>save <Kbd keys={['⌘', 'S']} className="ml-1 opacity-70" /></Button>
                <Button size="sm" variant="outline" className="h-8 text-xs" busy={busy} busyLabel={<><RefreshCw aria-hidden /> reloading…</>}
                  onClick={() => { setBusy(true); window.setTimeout(() => setBusy(false), 1600) }}><RefreshCw /> reload</Button>
                <span className="font-mono text-[11px] text-muted-foreground">{busy ? 'aria-busy · clicks ignored · focus kept' : 'press one'}</span>
              </div>
            </Demo>
          </Block>

          <Block id="copy-action" title="CopyAction" api={`<CopyAction value={link}>
  <LinkIcon /> copy link
</CopyAction>

// value may be a function, built at press time
<CopyAction value={() => writeQuery(tokens)}>copy query</CopyAction>`}
            rule={<>
              <p>
                A copy answers <em>on the control that was pressed</em>: “copied” for two seconds, or “couldn't copy” in red
                with the reason on hover. The eye is already on the button; a toast lands in a corner the reader is not
                looking at, and it says “copied” whether or not anything reached the clipboard.
              </p>
              <Rule state="ok">The idle label sets the width and stays in the layout invisibly while the answer shows, so the row never moves.</Rule>
              <Rule state="ok">A failure is never dressed as a success. Plain http on a LAN address has no <code>navigator.clipboard</code>; the control says so, in red, and the reader selects the text instead.</Rule>
              <Rule state="ok">The answer is a polite live region, so a screen reader hears “copied” without the focus moving.</Rule>
              <Rule state="error">A toast for a copy. <code>notify('ok', 'link copied')</code> is the control’s job, done in the wrong place.</Rule>
            </>}>
            <Demo label="press one · the control answers in place">
              <div className="flex flex-wrap items-center gap-3">
                <CopyAction value={`${window.location.origin}/op-components#copy-action`}><LinkIcon aria-hidden className="h-3 w-3" /> copy link</CopyAction>
                <CopyAction className="h-7 gap-1.5 px-2.5 text-xs" value={() => `level:error service:billing-worker since:${new Date().toISOString().slice(0, 10)}`}><Search aria-hidden className="h-3.5 w-3.5" /> copy query</CopyAction>
                <span className="font-mono text-[11px] text-muted-foreground">copied · 2s · back to the verb</span>
              </div>
            </Demo>
          </Block>

          <Block id="kbd" title="Kbd" api={`<Kbd keys={['⌘', '⏎']} />   ⌘⏎ on macOS, Ctrl⏎ elsewhere
<Kbd keys="j" />`}
            rule={<><p>Key badge, platform-aware. Lives inside primary buttons, in ledger footers, next to inputs. Always an accelerator, never the only entry point.</p></>}>
            <Demo label="in context">
              <div className="flex flex-wrap items-center gap-3 text-xs">
                <Button size="sm" className="op-primary h-8 text-xs"><Rocket /> deploy <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button>
                <span className="text-muted-foreground"><KbdPair keys={['j', 'k']} does={['down', 'up']} className="mx-1" /> · <Kbd keys="⏎" className="mx-1" /> open · <Kbd keys="/" className="mx-1" /> filter</span>
              </div>
            </Demo>
          </Block>

          <Block id="echo" title="EchoDialog" api={`<EchoDialog
  trigger={<Button>roll back</Button>}
  echo="$ temps deploy rollback api-gateway --to dep_90e"
  title="Roll back" description="…" confirmWord="dep_90e"
  steps={['verify image', 'start containers', 'health check', 'switch routes', 'drain']}
  onDone={…} destructive? />`}
            rule={<><p>Every destructive or irreversible action. Three parts: a description that names what is lost and what is kept, typed confirmation with the resource name in a copyable badge right before the input, step progress that mirrors the backend. There is no other confirm dialog.</p></>}>
            <Demo label="rollback and delete share the component">
              <div className="flex flex-wrap gap-2">
                <EchoDialog trigger={<Button variant="outline" size="sm" className="h-8 text-xs"><RotateCcw /> roll back</Button>} echo="$ temps deploy rollback api-gateway --to dep_90e" title="Roll back" description="Routes production traffic back to dep_90e. About 5 seconds, no downtime." confirmWord="dep_90e" steps={['verify dep_90e image present', 'start dep_90e containers', 'health check /healthz', 'switch proxy routes', 'drain dep_91a']} onDone={() => setLog((l) => ['rolled back', ...l])} />
                <EchoDialog destructive trigger={<Button variant="outline" size="sm" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> delete project</Button>} echo="$ temps project delete api-gateway --keep-data" title="Delete project" description="Stops the project and removes routes and certificate. Database and backups stay." confirmWord="api-gateway" steps={['stop containers', 'remove proxy routes', 'revoke certificate', 'archive project record']} onDone={() => setLog((l) => ['deleted', ...l])} />
              </div>
            </Demo>
          </Block>

          <Block id="chart" title="TimeChart · RangePicker · ChartFooter" api={`<RangePicker ranges value onChange retentionDays={30} retentionLabel="30d" onGated={notify} />
<TimeChart data series={[{ key: 'req', name: 'requests' }]}
  markers={[{ id: 'dep_91a', x: '16:00' }]} hot onHot
  sampled={{ from: '14:00', to: '23:00', label: 'sampled 1 in 4' }} />
<ChartFooter>showing 24h · retention 30d · ┆ deploy</ChartFooter>`}
            rule={<><p>Every time axis carries deploy markers, the sampled window if any, and the retention horizon in the footer. Linear lines, ink on paper, no fills, no animation. The readout above works on touch.</p><p>Deploys land in bursts. Markers whose labels would overlap collapse into one label, "3 deploys", while every deploy keeps its own dotted line. Click the label for a strip listing the members; hover one to light its line, click to open it.</p><p>Ranges beyond retention are struck through, not hidden, and say which plan keeps them.</p></>}>
            <Demo label="Starter plan · 30d retention · sampled since 14:00 · three deploys in an hour collapse into one label · drag across the plot to select a window">
              <div className="space-y-2">
                <div className="flex items-center gap-2"><span className="op-label">requests / hour</span>
                  <RangePicker className="ml-auto" ranges={[{ label: '24h', days: 1 }, { label: '7d', days: 7 }, { label: '30d', days: 30 }, { label: '90d', days: 90 }]} value={range} onChange={setRange} retentionDays={30} retentionLabel="30d" onGated={(r) => setGate(`${r.label} is beyond 30d retention on Starter · Team keeps 90d`)} />
                </div>
                <TimeChart data={SERIES} unit="req" yTicks={[0, 500, 1000]} xInterval={5} series={[{ key: 'req', name: 'requests' }]} markers={[{ id: 'dep_90e', x: '09:00', at: '09:12', note: 'perf(router): cache edge lookups' }, { id: 'dep_91a', x: '16:00', at: '16:04', note: 'feat(checkout): new address form' }, { id: 'dep_91b', x: '17:00', at: '16:31', note: 'fix(checkout): null id on address form' }, { id: 'dep_91c', x: '18:00', at: '17:10', note: 'fix(checkout): retry stripe webhooks' }]} onOpen={(id) => setLog((l) => [`open ${id}`, ...l])} onSelect={(r) => setLog((l) => [r ? `select ${r.from} → ${r.to}` : 'clear selection', ...l])} hot={hot} onHot={setHot} sampled={{ from: '14:00', to: '23:00', label: 'sampled 1 in 4' }} />
                <ChartFooter><span>showing {range}</span><span>· retention 30d</span><span>· ┆ deploy</span><span>· ◌ sampled since 14:00</span>{gate && <span className="text-warning">· {gate}</span>}</ChartFooter>
              </div>
            </Demo>
          </Block>

          <Block id="ledger" title="Ledger (template)" api={`<Ledger status={<StatusLine …/>}
  columns={[{ label: 'project', key: 'name' }, 'status', { label: 'visitors', key: 'visitors', numeric: true }, …]}
  grid="1.4fr 1fr 120px 100px"
  rows={rows} total={n} filter={q} onFilter={setQ} placeholder="filter projects"
  hint="needs attention first" action={<Button>new</Button>} dense={false}
  state={loadingOrError ? <PageState …/> : undefined}
  page={{ page, pageSize: 20, total: 1284, onPage, onPageSize }} />   footer: 1–20 of 1,284 · ‹ prev · next › · 20 per page

// no filter props → no search box and no "/"; footer is extra text beside the pager, never instead of it
<Ledger status={null} columns={…} grid="…" rows={rows} total={n} dense={false} footer={<span>…</span>} />
rows={[{ id, state, icon: <Box />, cells, mobile, onOpen }]}   // icon = the row's kind`}
            rule={<><p>Status line, filter (<code>/</code>), actions, rows with <code>j</code>/<code>k</code>/<code>⏎</code>, footer with counts and keys. Rows sort by attention first. Pass a <code>PageState</code> as <code>state</code> and the rows are replaced.</p><p>The search box is drawn only when it is wired: pass <code>filter</code> and <code>onFilter</code> and you get the box and the <code>/</code> binding; omit them and neither exists. When <code>page</code> is set the footer is always the pager, and <code>footer</code> is extra text beside it. Text filters match case-insensitively.</p><p>Columns with a <code>key</code> are sortable: click the header to cycle ascending → descending → off, which returns to the default attention-first order. One column at a time; the footer says what is sorted and offers <em>clear</em>. Numeric columns right-align and sort as numbers; empty values sort last either way.</p><p>Projects, databases, deploys, domains, users, backups, errors are all this.</p></>}>
            <Demo label="live · try j k ⏎ /">
              <Ledger status={<StatusLine sticky={false} state="warn"><Phrase>api-gateway</Phrase> error rate 0.61% since dep_91a.</StatusLine>} columns={[{ label: 'project', key: 'name' }, 'status', { label: 'visitors · 24h', key: 'visitors', numeric: true }, { label: 'error rate', key: 'err', numeric: true }]} grid="1.4fr 1.4fr 120px 100px" rows={rows} total={3} filter={q} onFilter={setQ} placeholder="filter projects" hint="needs attention first" dense={false} />
              {log.length > 0 && <p className="mt-2 font-mono text-[11px] text-muted-foreground">last action: {log[0]}</p>}
            </Demo>
            <Demo label="paginated · the footer is the pager · try [ ] and the page size">
              <PagedLedgerDemo />
            </Demo>
          </Block>

          <Block id="inspector" title="Inspector" api={`<Inspector open={row} onClose={() => setOpen(null)}
  state="error" word="error" title="health check GET /healthz timed out"
  meta="billing-worker · production · 31m ago"
  onOpen={openRecord} copyLink={link}   // the control copies and says "copied" itself
  panes={[{ id: 'fields', title: 'Fields' }, { id: 'trace' }, { id: 'request' }, { id: 'context' }]}>
  <Section title="Fields">…</Section>
</Inspector>

// xl: ~520px, pushes the main column · below xl: an overlay sheet with a scrim
// below md: full screen · role="complementary"`}
            rule={<>
              <p>A right-hand panel that inspects a ledger row <em>beside</em> the list. A <b>Detail</b> is a page you navigate to and come back from; an <b>Inspector</b> is the row read without leaving the list, which is what a reader walking a log or an error list actually wants. About 520px, pushing the main column on <code>xl</code>; an overlay sheet over a scrim below <code>xl</code>; full screen below <code>md</code>. An ink rule on the left edge is the whole separation — no card, no shadow.</p>
              <p>The header is the row's identity in one line: the state glyph and its word, the title in mono, the meta under it, and three actions — <em>open</em> (go to the full record), <em>copy link</em>, and <code>×</code>. Under it a small in-panel toc row (<code>fields · trace · request · context</code>, numbered <code>1</code>–<code>4</code>) so the panel's own sections are reachable without scrolling it. The body is stacked <code>Section</code>s, the same ones a record page uses.</p>
              <p>The keyboard contract is the reason it exists: <Kbd keys={['⏎']} /> opens the cursor row, <KbdPair keys={['j', 'k']} does={['down', 'up']} /> keep moving the <em>ledger's</em> cursor with the panel following, <Kbd keys={['esc']} /> closes it and returns focus to the row it came from, and <Kbd keys={['/']} /> still reaches the query bar. Focus enters the panel only with <Kbd keys={['Tab']} />: it is not a dialog and never traps.</p>
              <Rule state="ok"><code>role="complementary"</code>, labelled by the row it is inspecting.</Rule>
              <Rule state="error">A modal dialog. A modal takes the list away, which is the one thing the reader is keeping.</Rule>
              <Rule state="error">Stealing focus on open, or letting <code>j</code>/<code>k</code> scroll the panel instead of moving the cursor.</Rule>
            </>}>
            <Demo label="live · open a row, then close it with ×">
              <InspectorDemo />
            </Demo>
          </Block>

          <Block id="detail" title="Detail (template) · PageTitle · Segmented" api={`<Detail title="acme-web" meta="production · dep_91a · main"
  status tabs={['overview','deploys','logs'] as const} tab onTab actions>
  {body}
</Detail>
<PageTitle title meta mark? />   // also standalone; mark is the identity mark (favicon, provider logo), never a flex box inside title: that drags the meta off the text baseline
<Segmented options={[['today','today'],['yesterday','vs yesterday']]} value onChange />`}
            rule={<><p>Title, status line, tabs with number keys, actions on the right, body. The title is identity (what am I looking at, and the one or two facts that place it); the breadcrumb in the shell is only navigation. One chart with markers, a metric grid, and one raised element (<code>.op-raise</code>) per screen.</p></>}>
            <Demo label="live · press 1 2 3">
              <Detail title="acme-web" meta="production · dep_91a · main" status={<StatusLine sticky={false} state="warn" more={{ label: '+1 warning', items: [{ state: 'warn', children: <>Certificate for acme.sh expires in 6 days. <Phrase>Renew</Phrase></> }] }}><Phrase>Error rate 0.61%</Phrase> since dep_91a.</StatusLine>} tabs={['overview', 'deploys', 'logs'] as const} tab={tab} onTab={setTab}
                actions={<>{tab === 'overview' && <Segmented options={[['today', 'today'], ['yesterday', 'vs yesterday']] as const} value={seg} onChange={setSeg} />}<Button size="sm" className="op-primary h-8 text-xs"><Rocket /> deploy <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button></>}>
                {tab === 'overview' && (
                  <MetricGrid cols={4}>
                    <Metric label="requests · 24h" value={30812} baseline={seg === 'yesterday' ? '+12% vs yesterday' : '+9% since dep_91a'} state="ok" />
                    <Metric label="error rate" value="0.61" unit="%" baseline="+0.2pt since dep_91a" state="warn" />
                    <Metric label="p95 latency" value={184} unit="ms" baseline="−9ms" state="ok" />
                    <Metric label="uptime · 90d" value="99.94" unit="%" baseline="2 incidents" state="ok" />
                  </MetricGrid>
                )}
                {tab === 'deploys' && (
                  <div className="op-rows border text-xs">
                    {[['dep_91a', 'e4d1f0a', 'fix: retry stripe webhooks', '41m ago', 'ok'], ['dep_90e', 'b7c9d21', 'feat: address autocomplete', '6h ago', 'ok'], ['dep_88f', 'c0ffee1', 'chore: bump deps', 'yesterday', 'ok']].map(([id, sha, msg, at, st]) => (
                      <div key={id} className="op-row grid grid-cols-[90px_minmax(0,1fr)_80px] items-center gap-3 md:grid-cols-[90px_80px_minmax(0,1fr)_80px]">
                        <Status state={st as State} label={id} /><span className="hidden font-mono text-muted-foreground md:inline">{sha}</span><span className="truncate">{msg}</span><span className="text-right text-muted-foreground">{at}</span>
                      </div>
                    ))}
                  </div>
                )}
                {tab === 'logs' && (
                  <pre className="op-inset overflow-x-auto border p-3 font-mono text-[11px] leading-5">{`14:02:11.412  INFO  http  GET /api/products 200 38ms
14:02:11.688  INFO  http  POST /checkout 200 412ms
14:02:12.004  WARN  stripe  webhook retry 2/5 evt_3Q1
14:02:12.310  ERROR checkout  TypeError: cannot read 'line1' AddressForm.tsx:88
14:02:12.311  INFO  http  POST /checkout 500 56ms`}</pre>
                )}
              </Detail>
            </Demo>
          </Block>

          <Block id="picker" title="Picker (searchable select)" api={`<Picker value onChange options={BRANCHES} label="auto-deploy branch"
  allowCustom="use branch" searchPlaceholder="filter 9 branches" />
<Picker value onChange options={ENVS} placeholder="choose an environment" />
<Picker … loading="branches from github.com/acme/web" />
<Picker … error="GitHub 401: token expired" onRetry />
<Picker … options={[{ value, label, meta, state, icon: <GitBranch /> }]} />   // icon = the option's kind`}
            rule={<><p>Anything with more than about seven options, or options the operator recognises rather than recalls (branches, images, regions, environments), is a Picker, never a plain <code>select</code>. Mono trigger the height of an Input; opens to a filter box and grouped rows with a muted <code>meta</code> (last commit, region). The current value is ●.</p><p>An option carries its <em>kind</em> in <code>icon</code>, a fixed 16px slot of muted ink before the label, separate from the glyph slot: required wherever the options are of different kinds (worktree · shared checkout · sandbox), left off when they are all the same. An icon is never tinted by <code>state</code>.</p><p><code>allowCustom</code> offers "use &lt;typed&gt;" for a branch that does not exist yet. Loading and error are states inside the list that say what was being fetched and from where, with a retry, because the operator has no one to ask why the branch list is empty.</p></>}>
            <Demo label="branches · grouped by recency · type to filter · try a new name">
              <div className="grid gap-4 sm:grid-cols-2">
                <Picker value={branch} onChange={setBranch} options={BRANCHES} label="auto-deploy branch" allowCustom="use branch" searchPlaceholder="filter 9 branches" />
                <Picker value={env} onChange={setEnv} placeholder="choose an environment" mono={false} options={[
                  { value: 'production', state: 'ok', meta: 'api.acme.sh' },
                  { value: 'staging', state: 'warn', meta: '1 deploy ahead' },
                  { value: 'pr-212', state: 'idle', meta: 'preview · sleeping', group: 'previews' },
                  { value: 'pr-209', state: 'idle', meta: 'preview · sleeping', group: 'previews' },
                ]} />
              </div>
            </Demo>
            <Demo label="mixed kinds · the icon says what the option is, the glyph how it is — two slots, never one">
              <div className="grid gap-4 sm:grid-cols-2">
                <Picker value={space} onChange={setSpace} label="workspace" mono={false} width="380px" options={[
                  { value: 'worktree', label: 'worktree · feat/checkout-address', meta: 'isolated', state: 'ok', icon: <GitBranch /> },
                  { value: 'main', label: 'main checkout', meta: 'shared with you', state: 'warn', icon: <Box /> },
                  { value: 'sandbox', label: 'sandbox · sbx_9f3', meta: 'docker · fsn1', state: 'ok', icon: <Container /> },
                ]} />
              </div>
            </Demo>
            <Demo label="loading and error are inside the list">
              <div className="grid gap-4 sm:grid-cols-2">
                <Picker value={null} onChange={() => undefined} options={[]} placeholder="auto-deploy branch" loading="branches from github.com/acme/web" />
                <Picker value={null} onChange={setBranch} options={pickState === 'ok' ? BRANCHES : []} placeholder="auto-deploy branch" allowCustom="use branch"
                  loading={pickState === 'loading' && 'branches from github.com/acme/web'}
                  error={pickState === 'error' && 'GitHub returned 401 for acme/web: the installation token expired. Reconnect the provider under Source › Git providers.'}
                  onRetry={() => { setPickState('loading'); window.setTimeout(() => setPickState('ok'), 900) }} />
              </div>
            </Demo>
          </Block>

          <Block id="settings" title="Settings (template) · Field" api={`<Settings status sections={[{ title, body }]} dirty onSave
  danger={<EchoDialog destructive …/>} />
<Field label="auto-deploy branch" help="every push builds">
  <Input … />
</Field>`}
            rule={<><p>Sections with a side index, a sticky save bar that <Kbd keys={['⌘', 'S']} /> clicks (so the button's pressed and disabled states are honest), and a danger zone whose only action is an EchoDialog.</p></>}>
            <Demo label="live · edit, then ⌘S">
              <Settings status={null} dirty={form.branch !== saved.branch} onSave={() => { setSaved(form); setLog((l) => ['saved', ...l]) }}
                sections={[{ title: 'deploy', body: <Field label="auto-deploy branch" help="every push to this branch builds and deploys"><Picker value={form.branch} onChange={(b) => setForm({ branch: b })} options={BRANCHES} allowCustom="use branch" searchPlaceholder="filter 9 branches" /></Field> }]}
                danger={<div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Delete this project</p><p className="text-[11px] text-muted-foreground">Database and backups are kept.</p></div><EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> delete</Button>} echo="$ temps project delete docs --keep-data" title="Delete project" description="Stops docs and removes routes." confirmWord="docs" steps={['stop containers', 'remove routes', 'archive record']} onDone={() => setLog((l) => ['deleted docs', ...l])} /></div>} />
            </Demo>
          </Block>

          <Block id="mark" title="ProjectMark" api={`<ProjectMark name icon? size={16 | 24} />
served from /api/projects/{id}/icon · fetched after a deploy · monogram until then`}
            rule={<>
              <p>A project's favicon or logo, where its name is and nowhere else: 16px in a row, list, palette or breadcrumb, 24px beside a page title. Never a tile, never a hero. It may keep its own colours; at that size it cannot compete with a state glyph, and a mark the reader recognises is worth more than palette purity.</p>
              <p>The fallback is a monogram in ink with a 1px border: first letter, mono. It is what you see while nothing is known, when the fetch fails, and for a project with no domain yet. Never a random colour, so "unknown" looks unknown.</p>
              <Rule state="ok">The console fetches the icon from the project's production domain after a successful deploy and serves it from its own origin. The browser never hot-links the project's host.</Rule>
              <Rule state="error">Coloured monogram avatars, a 40px logo tile at the top of the project page, or fetching favicons client-side from the project's domain.</Rule>
            </>}>
            <Demo label="in a row · 16px, with and without an icon">
              <div className="op-rows border bg-background text-xs">
                {[['acme-storefront', 'data:image/svg+xml;utf8,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" fill="#e4572e"/><circle cx="16" cy="16" r="7" fill="#fff"/></svg>')], ['billing-worker', undefined], ['acme-web', undefined]].map(([n, i]) => (
                  <div key={n} className="flex items-center gap-2 px-3 py-1.5"><ProjectMark name={String(n)} icon={i} /><span className="font-medium">{n}</span><span className="ml-auto font-mono text-muted-foreground">production</span></div>
                ))}
              </div>
            </Demo>
            <Demo label="beside a page title · 24px"><h1 className="op-title flex items-center gap-2"><ProjectMark name="billing-worker" size={24} />billing-worker</h1></Demo>
          </Block>

          <Block id="breakdown" title="Breakdown · Sparkline · Funnel · Flow" api={`<Breakdown rows={[{ label, count, icon?, children?, onOpen?, state? }]} total unit limit more percent />
<GeoMap rows={[{ geo, label, value, state, note? }]} onOpen? />
<Sparkline points={number[]} state? fill? />
<Funnel steps={[{ name, count, avgSeconds? }]} dropAlert=50 />
<Flow rows={[{ from?, to?, count, share }]} />
<TimeChart thresholds={[{ y, label, state }]} />`}
            rule={<>
              <p>The analytics forms. A <b>Breakdown</b> is one dimension ranked: label, count, share, and the share as an ink bar behind the row. Rows with children open in place (country → region → city) and the header shows the way back; the remainder that did not make the top-N is one honest "other" row. An optional <b>icon</b> says what kind of thing the row is (a flag, a channel, a device) in a fixed 16px slot; state stays with the glyph, never with the icon. A <b>GeoMap</b> is the same rows as a by-country list drawn on the world: countries filled by state only (no ten-bin gradient, no floating tooltip), on desktop the hovered country reads at the pointer and nothing sits under the map; on a phone a row under the map is the reader (tap to read, tap again to open), and countries without samples stay muted. The list is the default view and carries the keyboard; the map is the second view in a Segmented.</p>
              <p>A <b>Sparkline</b> is a trend in the width of a cell: one ink line, no axes, the last point marked, never a number of its own. A <b>Funnel</b> is bars by share of entrants with conversion and drop-off under each; the drop-off is the number that matters and the only one that turns red. A <b>Flow</b> is "from → to" ranked, not a Sankey: readable at any width and sortable.</p>
              <Rule state="ok">Ink bars, mono numbers, one soft rule per row, one frame per group. The bar is relative to the largest row so shape is comparable; the percentage is relative to the total so it is honest.</Rule>
              <Rule state="error">Pie charts, coloured bars per row, a globe, a Sankey, a sparkline with its own y-axis or tooltip.</Rule>
            </>}>
            <Demo label="Breakdown · click United States, then California"><Breakdown rows={VZ_LOCATIONS} total={12418} limit={6} more={{ label: 'all countries', onClick: () => setLog((l) => ['open dimension: country', ...l]) }} /></Demo>
            <div className="op-grid grid gap-6 md:grid-cols-2"><Demo label="Breakdown · channel icons, ◌ counted apart"><Breakdown rows={VZ_CHANNELS} total={12_330} /></Demo><Demo label="Breakdown · device icons"><Breakdown rows={VZ_DEVICES} total={12_330} /></Demo></div>
            <Demo label="GeoMap · the second view of a by-country list; filled by state, read at the pointer, or in the row under it on a phone"><GeoMap rows={VZ_GEO} /></Demo>
            <Demo label="Sparkline · in a row, next to its number">
              <div className="op-rows border bg-background text-xs">
                {[['/', 9812, 1], ['/pricing', 4120, 4], ['/blog/self-hosted-vercel', 2890, 9]].map(([p, n, seed]) => (
                  <div key={String(p)} className="grid grid-cols-[minmax(0,1fr)_8rem_5rem] items-center gap-3 px-3 py-1.5"><span className="truncate font-mono">{p}</span><Sparkline points={Array.from({ length: 24 }, (_, i) => 20 + Math.abs(Math.sin((i + Number(seed)) / 3.5)) * 60)} state={p === '/blog/self-hosted-vercel' ? 'warn' : undefined} /><Num value={Number(n)} className="text-right" /></div>
                ))}
              </div>
            </Demo>
            <Demo label="Funnel · drop-off ≥ 50% is red"><Funnel steps={[{ name: 'Viewed /pricing', count: 3480 }, { name: 'Clicked "Start free"', count: 1240, avgSeconds: 48 }, { name: 'Created account', count: 910, avgSeconds: 95 }, { name: 'Connected a repository', count: 402, avgSeconds: 310 }, { name: 'First deploy', count: 318, avgSeconds: 640 }]} /></Demo>
            <Demo label="Flow · transitions, entries and exits are the same rows"><Flow rows={[{ from: '/', to: '/pricing', count: 2210, share: 36 }, { from: '/pricing', to: '/download', count: 1310, share: 38 }, { to: '/blog/self-hosted-vercel', count: 2710, share: 22 }, { from: '/download', count: 1610, share: 88 }]} /></Demo>
          </Block>

          <Block id="callout" title="Callout" api={`<Callout state title quote? action?>consequence</Callout>`}
            rule={<>
              <p>An alert inside a page. A <b>StatusLine</b> is one sentence that rolls up into the header count; a <b>Callout</b> is for a fault whose evidence belongs where it applies. Glyph and title in the state colour, a 2px left rule in the same colour, the other system's message quoted in mono (never paraphrased), one sentence of consequence and what the action changes, and the action. If nothing is wrong, nothing renders: a Callout is never decoration.</p>
            </>}>
            <div className="space-y-4">
              <Demo label="error · with the quoted message and the one action"><Callout state="error" title="acme-org is disconnected: the GitHub App was suspended" quote="GitHub returned 401: installation access token expired; the app was suspended by an org admin on 2026-09-03" action={<Button size="sm" className="op-primary h-7 text-xs">reconnect acme-org</Button>}>Pushes to acme-org repositories have not deployed for 2 days; 31 repositories are affected. Reconnect re-authorizes the app on GitHub; nothing else changes.</Callout></Demo>
              <Demo label="warn · no quote, consequence only"><Callout state="warn" title="The nightly backup has not run since 3d ago" action={<Button size="sm" variant="outline" className="h-7 text-xs">check the schedule</Button>}>Until it runs, a restore loses everything after the last backup.</Callout></Demo>
              <Demo label="ok · after the fix, stays until dismissed by the next check"><Callout state="ok" title="acme-org reconnected">31 repositories deploy on push again. The next health check runs in 10 minutes.</Callout></Demo>
            </div>
          </Block>
          <Block id="strip" title="StatusStrip · ScoreRing · CalendarHeatmap · Live" api={`<StatusStrip buckets={[{ start, state, checks, down, p50_ms, p95_ms }]} height />
<ScoreRing value={0–100} label />
<CalendarHeatmap days={[{ date, count, ids? }]} unit="deploys" onOpen />
<Live every="30s" paused onToggle />`}
            rule={<>
              <p>A <b>StatusStrip</b> is uptime as shape: one segment per bucket coloured by state, the legend is the five glyphs, hover reads the bucket. It fills its cell so monitors compare by shape. A <b>ScoreRing</b> is a 0–100 score as an arc with the number in the middle; the arc colour is the state at the Web Vitals thresholds (≥90 ok, ≥50 warn). A <b>CalendarHeatmap</b> is activity per day in five ink intensities: ink, because the colour means how much, not how well. Its readout is the point: a cell used to carry a <code>title</code> attribute, which a pointer gets, a touch never does and a keyboard cannot reach. Now the grid is one tab stop, arrows walk it, <code>⏎</code> opens the day through <code>onOpen</code>, and the readout — the date, the count with its <code>unit</code>, and the <code>ids</code> of what actually shipped — renders as a live region beside the cursor on a pointer and as a fixed row under the grid on a phone.</p>
              <p><b>Live</b> says a surface updates by itself, with the interval, and can be paused. It sits in a ledger's footer or a section's meta; a page never polls silently.</p>
              <Rule state="ok">States only through the state colours; quantity only through ink intensity or length.</Rule>
              <Rule state="error">A readout that only a hover can reach. <code>title</code> is not a readout.</Rule>
              <Rule state="error">A green heatmap, a gradient ring, a status strip with a number inside every segment.</Rule>
            </>}>
            <Demo label="StatusStrip · hover 20:30"><div className="space-y-3 border bg-background p-3 text-xs">{([['acme.sh', VZ_STRIP.map((b) => ({ ...b, state: 'ok' as const, down: 0 })), 100], ['api-gateway', VZ_STRIP, 97.9]] as [string, StatusBucket[], number][]).map(([n, b, up]) => <div key={n} className="grid grid-cols-[8rem_minmax(0,1fr)_4rem] items-center gap-3"><span className="font-medium">{n}</span><StatusStrip buckets={b} height={16} /><Num value={up} unit="%" className="text-right" /></div>)}</div></Demo>
            <Demo label="ScoreRing · web vitals"><div className="flex flex-wrap gap-6 border bg-background p-3">{[['LCP', 92], ['INP', 96], ['CLS', 88], ['TTFB', 71], ['FCP', 44]].map(([k, v]) => <ScoreRing key={String(k)} value={Number(v)} label={String(k)} />)}</div></Demo>
            <Demo label="CalendarHeatmap · 12 weeks of deploys, and the readout every input can reach">
              <div className="border bg-background p-3">
                <CalendarHeatmap days={VZ_DAYS} unit="deploys" onOpen={(d) => setLog((l) => [`open ${d.date} · ${d.ids?.length ? d.ids.join(' · ') : `${d.count} deploys`}`, ...l])} />
              </div>
              <p className="mt-2 font-mono text-[10px] text-muted-foreground">
                hover or focus a day to read it · ← → move a week, ↑ ↓ move a day, ⏎ opens · on a phone the readout is the row under the grid
              </p>
            </Demo>
            <Demo label="Live"><div className="border bg-background px-3 py-2"><Live every="30s" paused={livePaused} onToggle={() => setLivePaused((p) => !p)} /></div></Demo>
          </Block>

          <Block id="trace" title="Waterfall · StackTrace" api={`<Waterfall spans={[{ id, name, service, start_ms, duration_ms, state?, children? }]} total_ms selected onSelect />
<StackTrace frames={[{ fn, file, line, col?, inApp?, original?, context?: [{ line, code }] }]} />`}
            rule={<>
              <p>A <b>Waterfall</b> is the spans of one trace: a collapsible tree on the left, bars placed by offset and width against the whole trace on the right, the duration in mono at the bar's end. Error spans get × and a red bar; everything else is ink. Selecting a span is the page's business (its attributes open beside it).</p>
              <p>A <b>StackTrace</b> is frames most-recent first. In-app frames are ink and open with their source context (gutter, the failing line marked ×); vendor frames are muted and closed. A symbolicated frame shows the original file in the corner.</p>
              <Rule state="ok">One frame around the whole list, soft rules between frames, the code in the inset pane.</Rule>
              <Rule state="error">A card per frame, a colour per service, a flame graph where a tree will do.</Rule>
            </>}>
            <Demo label="Waterfall · collapse stripe.charge"><Waterfall spans={VZ_SPANS} total_ms={812} selected={span} onSelect={(s) => { setSpan(s.id); setLog((l) => [`selected ${s.name}`, ...l]) }} /></Demo>
            <Demo label="StackTrace · in-app frames open"><StackTrace frames={VZ_FRAMES} /></Demo>
          </Block>

          <Block id="logs" title="LogLines · Stages · Histogram" api={`<LogLines lines={[{ t, level, source?, msg }]} live height search? />
<Stages stages={[{ name, state, duration?, lines?, result?, phase? }]} />
<Histogram buckets={[{ le, count }]} unit value={pct} onChange />`}
            rule={<>
              <p><b>LogLines</b> is the row of a log: time in the gutter, level as a glyph (× error, ◐ warn, nothing otherwise), source muted, the message in mono and wrapping. Levels are toggles above and the hidden count is said. Virtualisation and search belong to the console around it. <b>Stages</b> is a build in order: state glyph, name, what the step produced, duration; the running stage is open and streams its lines; finished stages open on click, one at a time. A step's line says its result ("798 assets · 18.8 MB"), never its description: the reader knows what "build image" is for, what they cannot know is what came out. Phases (build · release · after going live) are headers in the list, and steps after going live read muted because they do not hold traffic back.</p>
              <p>A <b>Histogram</b> is a distribution and the statistic picked from it: the selector is avg · p50 · p90 · p95 · p99, the chosen value is a red rule through the bars and buckets past it are muted so the tail is visible.</p>
              <Rule state="ok">The level glyph is the only colour in a log line besides red text for errors.</Rule>
              <Rule state="error">Rainbow ANSI by default, a stage list that opens every log at once, a percentile picker without the distribution behind it.</Rule>
            </>}>
            <Demo label="LogLines · toggle debug off"><LogLines lines={VZ_LOG} live height={180} /></Demo>
            <Demo label="Stages · build is running"><Stages stages={VZ_STAGES} /></Demo>
            <Demo label="Histogram · http.server.request.duration"><Histogram buckets={VZ_HIST} unit="ms" value={pct} onChange={setPct} /></Demo>
      </Block>

          <FormBlocks />
          <NotificationBlocks />
          <DataVizBlocks />
          {/* The second wave of the same subject, straight after the first. */}
          <DataVizBlocks2 />
          <GenUiBlocks />
          <ContentBlocks />

          <Block id="prose" title=".op-prose" api={`<div className="op-prose">{markdown}</div>

.op-lede           the lead paragraph
data-align="end"   a numeric column, mono + tabular
.op-raw            a live block: not prose, keeps its own type
--op-measure       the reading measure (68ch)`}
            rule={<>
              <p>The whole look of a body of long-form prose, as one class. A post, a docs page and a changelog entry get it by wrapping the body; <b>Article</b> applies it for you. There is one copy of these rules, so the guide, the docs site and the blog cannot drift apart one renderer at a time.</p>
              <p>Every rule is a <i>descendant</i> rule, so putting the class on a single paragraph still means only &ldquo;this wraps, so it is the sans face&rdquo;. The measure is capped inside the frame; figures, tables and code panes are allowed past it.</p>
              <Rule state="ok">Headings on the type ladder, ink links with an underline, square bullets, ledger tables, framed figures with counted captions.</Rule>
              <Rule state="error">A coloured heading, a blue link, a yellow <code>mark</code>, a rounded or shadowed image, a table that is not a ledger.</Rule>
            </>}>
            <Demo label="a body of prose">
              <div className="op-prose">
                <h2>What the proxy already knows</h2>
                <p className="op-lede">Assistants read the site more often than people do, and the proxy already sees every one of those requests.</p>
                <p>
                  Every request is written to <code>proxy_logs</code> with the host, the path, the status and the user agent. Nothing extra has to be
                  installed — which matters, because a crawler does not execute the analytics snippet. Press <kbd>/</kbd> to reach the query bar, and
                  the token you type is <mark>the whole filter</mark>.
                </p>
                <h3>The fields worth keeping</h3>
                <ul>
                  <li>Agent — the crawler&rsquo;s own name, verbatim.
                    <ul><li>Never re-cased: <code>GPTBot</code> is what it calls itself.</li></ul>
                  </li>
                  <li>Path, status, bytes out.</li>
                </ul>
                <ol>
                  <li>Filter the log by user agent.</li>
                  <li>Group by agent over thirty days.</li>
                </ol>
                <table>
                  <thead><tr><th scope="col">agent</th><th scope="col" data-align="end">requests</th><th scope="col" data-align="end">bytes out</th></tr></thead>
                  <tbody>
                    <tr><td>GPTBot</td><td data-align="end">184,220</td><td data-align="end">3.1 GB</td></tr>
                    <tr><td>ClaudeBot</td><td data-align="end">61,905</td><td data-align="end">1.1 GB</td></tr>
                  </tbody>
                </table>
                <blockquote><p>The crawler is not the audience. The person reading the answer that quotes you is.</p></blockquote>
                <pre><code>{'bunx @temps-sdk/cli logs query --group-by ua --format table'}</code></pre>
                <details><summary>Why the analytics page was flat</summary><p>A crawler does not run the snippet, so nothing about it reaches analytics.</p></details>
                <hr />
                <p>Everything else turned out to be detail we looked at once and never again.</p>
              </div>
            </Demo>
          </Block>

          <Block id="article" title="Article · CodeBlock · ImageFigure" api={`<Article title lede author={{ name, mark }} date readingMinutes
         toc backHref backLabel aside footer>{body}</Article>

<CodeBlock code lang filename? copy />
<ImageFigure src dark? alt caption width height />`}
            rule={<>
              <p><b>Article</b> is the fourth page template: a page that is read top to bottom rather than operated. Title on the display ladder, lede, byline (mark · name · absolute date · reading time), the body in <code>.op-prose</code>, and a right rail built from the body&rsquo;s <i>rendered</i> h2/h3 — sticky, current section in ink, every entry a real link. Under two headings the rail does not draw.</p>
              <p><b>CodeBlock</b> says the language and the filename and copies on itself; code in a document exists to be run. <b>ImageFigure</b> frames a picture at 1px, numbers its caption, and swaps <code>src</code> for <code>dark</code> with the theme.</p>
              <Rule state="ok">The worked example is <Link to="/v1-article" className="underline underline-offset-4">/v1-article</Link>; the rules are <Link to="/guide#content-pages" className="underline underline-offset-4">/guide#content-pages</Link>.</Rule>
              <Rule state="error">A hero illustration, a browser-chrome mockup, a screenshot on one ground only, a code block a reader has to retype.</Rule>
            </>}>
            <Demo label="Article · the shape">
              <div className="border">
                <Article
                  title="Track AI crawler traffic at the proxy"
                  lede="Assistants read the site far more often than people do."
                  author={{ name: 'maya', mark: <ProjectMark name="maya" size={16} /> }}
                  date="2026-09-01"
                  readingMinutes={6}
                  backHref="/v1-landing"
                  className="py-6"
                >
                  <h2>What the proxy already knows</h2>
                  <p>Every request is written with the host, the path, the status and the user agent.</p>
                  <h2>What the numbers said</h2>
                  <p>Four paths accounted for more than half of everything read.</p>
                </Article>
              </div>
            </Demo>
            <Demo label="CodeBlock · a fence that says what it is">
              <CodeBlock lang="bash" filename="deploy.sh" code={'bunx @temps-sdk/cli deploy --project acme-storefront --env production'} />
            </Demo>
            <Demo label="ImageFigure · framed, captioned, both grounds">
              <div className="op-prose">
                <ImageFigure
                  src="/figures/logs-light.png"
                  dark="/figures/logs-dark.png"
                  alt="The Temps logs screen: a volume chart above a list of request lines, with facets down the right."
                  caption="the logs screen · one figure, both grounds"
                  width={1440}
                  height={900}
                />
              </div>
            </Demo>
          </Block>

          <TokenBlocks />
    </DocPage>
  )
}

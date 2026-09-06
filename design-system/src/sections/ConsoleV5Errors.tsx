// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import { ArrowRight, BellOff, Bug, Check, Code, Globe, MousePointerClick, Terminal, User } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Breakdown, ChartFooter, Detail, EchoDialog, Ledger, Lede, Num, PageState, Phrase, ProjectMark, RangePicker, Section, Segmented, KeyValue, Status, Timeline, Columns, Sparkline, StackTrace, StatusLine, TimeChart,
  type Frame, type KV, type LedgerRow, type Range, type State,
} from '@/components/op'
import type { Notify, Plan } from './ConsoleV5Observe'
import { useFresh } from './console-fresh'
import { PROJECT_ICONS } from './console-projects'

/**
 * Errors, the Sentry shape with the noise removed. An issue is one row: the
 * exception and its message, where it happened, how many events and users,
 * a 24h sparkline, when it was last seen. State is a glyph and a word
 * (regressed, new, unresolved, resolved, ignored), never a coloured badge.
 * The issue record is the record recipe: verdict, lede, stack trace and
 * chart in the content column, tags and the latest event in the aside,
 * with the individual events as a facet.
 */

// ── Data ─────────────────────────────────────────────────────────────
type IssueState = 'regressed' | 'new' | 'unresolved' | 'resolved' | 'ignored'
type Issue = { id: string; type: string; message: string; culprit: string; fn: string; project: string; env: string; level: 'error' | 'warn'; state: IssueState; events24h: number; users: number; first: string; last: string; release: string; spark: number[]; handled: boolean; assignee?: string; resolvedIn?: string }
const sp = (seed: number, drop = false) => Array.from({ length: 24 }, (_, i) => Math.round(Math.abs(Math.sin((i + seed) / 3)) * 30 + (drop && i > 18 ? 0 : (i * seed) % 9)))
const ISSUES: Issue[] = [
  { id: 'i_4821', type: 'TypeError', message: "Cannot read properties of undefined (reading 'items')", culprit: 'src/checkout/Cart.tsx:41', fn: 'renderCart', project: 'acme-storefront', env: 'production', level: 'error', state: 'regressed', events24h: 1204, users: 312, first: '12d ago', last: '2m ago', release: 'dep_91a', spark: [2, 1, 0, 0, 1, 0, 0, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 40, 88, 120, 140, 132, 128], handled: false, resolvedIn: 'dep_88c' },
  { id: 'i_4830', type: 'ECONNRESET', message: 'socket hang up talking to upstream 10.0.3.4:8080', culprit: 'src/router.rs:212', fn: 'proxy_upstream', project: 'api-gateway', env: 'production', level: 'warn', state: 'new', events24h: 44, users: 0, first: '3h ago', last: '9m ago', release: 'dep_91a', spark: sp(2), handled: true },
  { id: 'i_4790', type: 'ZodError', message: 'Invalid input: expected string, received null at "email"', culprit: 'src/api/signup.ts:27', fn: 'validateSignup', project: 'acme-storefront', env: 'production', level: 'warn', state: 'unresolved', events24h: 96, users: 71, first: '6d ago', last: '31m ago', release: 'dep_90e', spark: sp(5), handled: true, assignee: 'jules' },
  { id: 'i_4701', type: 'ChunkLoadError', message: 'Loading chunk 412 failed (missing: /assets/pricing-8f1a.js)', culprit: 'src/routes/pricing.tsx:1', fn: 'lazy', project: 'acme-storefront', env: 'production', level: 'error', state: 'unresolved', events24h: 18, users: 18, first: '3d ago', last: '2h ago', release: 'dep_90e', spark: sp(7), handled: false },
  { id: 'i_4655', type: 'DeadlineExceeded', message: 'pg query exceeded 5s: SELECT * FROM orders WHERE customer_id = $1', culprit: 'src/db/orders.ts:88', fn: 'ordersByCustomer', project: 'api-gateway', env: 'production', level: 'warn', state: 'unresolved', events24h: 7, users: 5, first: '9d ago', last: '5h ago', release: 'dep_89b', spark: sp(3), handled: true },
  { id: 'i_4610', type: 'ReferenceError', message: 'gtag is not defined', culprit: 'src/analytics.ts:12', fn: 'track', project: 'docs', env: 'production', level: 'error', state: 'ignored', events24h: 210, users: 190, first: '20d ago', last: '1m ago', release: 'dep_80a', spark: sp(4), handled: false },
  { id: 'i_4502', type: 'TypeError', message: "Cannot read properties of null (reading 'focus')", culprit: 'src/ui/Dialog.tsx:66', fn: 'onOpen', project: 'acme-crm', env: 'production', level: 'error', state: 'resolved', events24h: 0, users: 0, first: '15d ago', last: '4d ago', release: 'dep_87f', spark: sp(1, true), handled: false, resolvedIn: 'dep_87f' },
]
const STATE_GLYPH: Record<IssueState, State> = { regressed: 'error', new: 'error', unresolved: 'warn', resolved: 'ok', ignored: 'idle' }
const FRAMES: Frame[] = [
  { fn: 'renderCart', file: 'src/checkout/Cart.tsx', line: 41, col: 18, inApp: true, original: 'assets/index-8f1a.js:2:41873', context: [{ line: 39, code: '  const cart = useCart()' }, { line: 40, code: '  const { data } = useQuery(cartQuery(cart.id))' }, { line: 41, code: '  return data.items.map((item) => <CartLine key={item.id} item={item} />)' }, { line: 42, code: '}' }] },
  { fn: 'CheckoutPage', file: 'src/checkout/CheckoutPage.tsx', line: 88, col: 9, inApp: true, context: [{ line: 87, code: '      <Summary />' }, { line: 88, code: '      {step === "cart" && renderCart()}' }, { line: 89, code: '      <PaymentForm />' }] },
  { fn: 'renderWithHooks', file: 'node_modules/react-dom/cjs/react-dom.production.js', line: 11121, inApp: false },
  { fn: 'updateFunctionComponent', file: 'node_modules/react-dom/cjs/react-dom.production.js', line: 14320, inApp: false },
  { fn: 'beginWork', file: 'node_modules/react-dom/cjs/react-dom.production.js', line: 15931, inApp: false },
]
const CRUMBS = [
  { t: '-14.2s', icon: <Globe />, label: 'navigation', note: '/pricing → /checkout' },
  { t: '-9.8s', icon: <ArrowRight />, label: 'GET /api/cart/c_8f21', note: '200 · 84 ms' },
  { t: '-3.1s', icon: <MousePointerClick />, label: 'click', note: 'button "Continue to payment"' },
  { t: '-3.0s', icon: <ArrowRight />, label: 'GET /api/cart/c_8f21', note: '204 · empty body · after dep_91a the cart endpoint returns 204 when the cart is empty', state: 'warn' as State },
  { t: '-0.4s', icon: <Terminal />, label: 'console.error', note: 'Warning: data is undefined' },
  { t: '0', icon: <Bug />, label: 'TypeError', note: "Cannot read properties of undefined (reading 'items')", state: 'error' as State },
]
const HOURLY = Array.from({ length: 48 }, (_, i) => ({ t: `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`, events: i < 40 ? (i % 7 === 0 ? 2 : 0) : [12, 61, 140, 188, 210, 199, 204, 190][i - 40], users: i < 40 ? (i % 7 === 0 ? 1 : 0) : [9, 40, 61, 70, 66, 58, 61, 55][i - 40] }))

// ── Issues ledger ────────────────────────────────────────────────────
const FILTERS = [['review', 'for review'], ['unresolved', 'unresolved'], ['regressed', 'regressed'], ['resolved', 'resolved'], ['ignored', 'ignored'], ['all', 'all']] as const
type Filter = (typeof FILTERS)[number][0]
const inFilter = (i: Issue, f: Filter) => f === 'all' ? true : f === 'review' ? i.state === 'regressed' || i.state === 'new' : f === 'unresolved' ? i.state !== 'resolved' && i.state !== 'ignored' : i.state === f

const RANGES: readonly Range[] = [{ label: '1h', days: 0.05 }, { label: '24h', days: 1 }, { label: '7d', days: 7 }, { label: '30d', days: 30 }, { label: '90d', days: 90 }]
export function ErrorsScreen({ dense, plan, notify, go }: { dense: boolean; plan: Plan; notify: Notify; go: (v: string) => void }) {
  const [fresh] = useFresh()
  const [q, setQ] = useState('')
  const [filter, setFilter] = useState<Filter>('review')
  const [range, setRange] = useState('24h')
  const [win, setWin] = useState({ from: '', to: '' })
  const rangeLabel = range === 'custom' ? `${win.from.replace('T', ' ')} → ${win.to.replace('T', ' ')}` : range
  // `?fail=1` keeps the outage demo: the error store itself is down, the page says which one and offers retry.
  const fail = typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('fail') === '1'
  const [phase, setPhase] = useState<'error' | 'retrying' | 'ok'>(fail ? 'error' : 'ok')
  const retry = () => { setPhase('retrying'); window.setTimeout(() => setPhase('ok'), 900) }
  const issues = fresh ? [] : ISSUES
  const list = issues.filter((i) => inFilter(i, filter)).filter((i) => !q || `${i.type} ${i.message} ${i.culprit} ${i.project}`.toLowerCase().includes(q.toLowerCase()))
  const worst = ISSUES[0]
  const rows: LedgerRow[] = list.map((i) => ({
    id: i.id, state: STATE_GLYPH[i.state], onOpen: () => go(`issue:${i.id}`),
    sort: { title: i.type, events: i.events24h, users: i.users, last: i.last, first: i.first },
    mobile: <>
      <span className="block truncate"><span className="font-medium">{i.type}</span> <span className="text-muted-foreground">{i.message}</span></span>
      <span className="mt-0.5 flex items-center gap-2 text-[11px] text-muted-foreground"><ProjectMark name={i.project} icon={PROJECT_ICONS[i.project]} /><span className="truncate font-mono">{i.culprit}</span><span className="ml-auto shrink-0 font-mono">{i.events24h.toLocaleString()} · {i.users} users · {i.last}</span></span>
    </>,
    cells: [
      <span className="block min-w-0">
        <span className="block truncate"><span className="font-medium">{i.type}</span> <span className="text-muted-foreground">{i.message}</span></span>
        {/* The row's colour (the sparkline) is anchored by a glyph and a word; the state is never a bare word. */}
        <span className="flex min-w-0 items-center gap-2 font-mono text-[11px] text-muted-foreground">
          <Status state={STATE_GLYPH[i.state]} label={i.state} className="shrink-0" />
          <span className="min-w-0 truncate">{i.state === 'regressed' ? `in ${i.release} · fixed in ${i.resolvedIn} · ${i.culprit}` : i.state === 'new' ? `in ${i.release} · ${i.culprit}` : i.state === 'resolved' ? `in ${i.resolvedIn} · ${i.culprit}` : i.assignee ? `${i.assignee} · ${i.culprit}` : i.culprit}</span>
        </span>
      </span>,
      <span className="flex items-center gap-1.5 text-muted-foreground"><ProjectMark name={i.project} icon={PROJECT_ICONS[i.project]} /><span className="truncate">{i.project}</span></span>,
      <span className="block w-full text-foreground"><Sparkline points={i.spark} state={i.state === 'regressed' ? 'error' : undefined} /></span>,
      <Num value={i.events24h} />, <Num value={i.users} />,
      <span className="text-muted-foreground">{i.last}</span>,
      <span className="text-muted-foreground">{i.first}</span>,
    ],
  }))
  const counts = { review: ISSUES.filter((i) => inFilter(i, 'review')).length, unresolved: ISSUES.filter((i) => inFilter(i, 'unresolved')).length }
  const status = fresh
    ? <StatusLine state="idle">No errors reported yet. No project has a DSN installed.</StatusLine>
    : phase !== 'ok' ? null
      : <StatusLine state="error" more={{ label: '+1 warning', items: [{ state: 'warn', children: <><Phrase onClick={() => go('issue:i_4830')}>ECONNRESET</Phrase> on api-gateway is new since dep_91a: 44 events in 3h, handled, no users affected.</> }] }}>
        <Phrase onClick={() => go(`issue:${worst.id}`)}>{worst.type} in {worst.culprit}</Phrase> regressed after dep_91a: {worst.events24h.toLocaleString()} events from {worst.users} users in 2h. It was fixed in {worst.resolvedIn}.
      </StatusLine>
  return (
    <Ledger
      title="Errors" meta={fresh ? 'all projects · no data yet' : `all projects · production · ${rangeLabel} · ${counts.review} for review · ${counts.unresolved} unresolved`}
      status={status} dense={dense}
      columns={[{ label: 'issue', key: 'title' }, 'project', range === 'custom' ? 'window' : range, { label: 'events', key: 'events', numeric: true }, { label: 'users', key: 'users', numeric: true }, { label: 'last seen', key: 'last' }, { label: 'first seen', key: 'first' }]}
      grid="minmax(18rem,3fr) minmax(8rem,1fr) minmax(6rem,1fr) minmax(64px,max-content) minmax(56px,max-content) minmax(72px,max-content) minmax(72px,max-content)"
      rows={rows} total={list.length} filter={q} onFilter={setQ} placeholder="filter by type, message, file, project"
      state={fresh ? (
        <PageState state="unconfigured" title="No errors reported yet"
          missing="a DSN in the app. Install the SDK, paste the project's DSN, and every unhandled exception arrives here grouped by stack trace with the release that introduced it."
          example={<div className="space-y-1 font-mono text-[11px]"><p>× TypeError Cannot read properties of undefined (reading 'items') · regressed in dep_91a · 1,204 events · 312 users</p><p>◐ ZodError Invalid input at "email" · jules · 96 events · 71 users</p><pre className="op-inset whitespace-pre-wrap border px-3 py-2 text-foreground">{`import * as temps from "@temps-sdk/browser"\ntemps.init({ dsn: "https://k9x@temps.acme.sh/1" })`}</pre></div>}
          settingsHref="/settings/errors" settingsLabel="copy the DSN" />
      ) : phase === 'ok' ? undefined : (
        <PageState state="error" title="Error store unreachable" message="connection refused: clickhouse://127.0.0.1:9000 (timeout 3s)" resource="clickhouse · events-ch" onRetry={retry} retrying={phase === 'retrying'} />
      )}
      hint="regressed and new first, then by events · × unresolved · ◐ handled · ● resolved · ○ ignored"
      action={<><RangePicker ranges={RANGES} value={range} onChange={setRange} retentionDays={plan.retentionDays} retentionLabel={plan.retention} onGated={(r) => notify('warn', `${r.label} is beyond this plan's retention`, `currently ${plan.retention}`)} custom={{ from: win.from, to: win.to, onChange: (from, to) => setWin({ from, to }) }} /><Segmented options={FILTERS} value={filter} onChange={setFilter} className="h-7 [&>button]:h-7" /></>}
      footer={<span>an issue is one stack trace across releases; events are its occurrences · sampled at 100% on this plan</span>}
    />
  )
}

// ── Issue record ─────────────────────────────────────────────────────
const TABS = ['overview', 'events', 'tags'] as const
type Tab = (typeof TABS)[number]
export function IssueScreen({ id, dense, notify, go }: { id: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const i = ISSUES.find((x) => x.id === id) ?? ISSUES[0]
  const [tab, setTab] = useState<Tab>('overview')
  const [q, setQ] = useState('')
  const [page, setPage] = useState(1)
  const [state, setState] = useState<IssueState>(i.state)
  const word = state === 'regressed' ? 'regressed' : state === 'new' ? 'new' : state === 'resolved' ? 'resolved' : state === 'ignored' ? 'ignored' : 'unresolved'
  const status = state === 'regressed'
    ? <StatusLine state="error">Fixed in {i.resolvedIn}, back since <Phrase onClick={() => go(i.project)}>{i.release}</Phrase>: the cart endpoint now returns 204 for an empty cart and <span className="font-mono">data</span> is undefined. {i.users} users hit it in 2h.</StatusLine>
    : state === 'resolved' ? <StatusLine state="ok">Resolved. No events since {i.last}; reopens automatically if it comes back in a later release.</StatusLine>
      : state === 'ignored' ? <StatusLine state="idle">Ignored. Still counted, never raised.</StatusLine>
        // The verdict says what to do about it; the counts are already lede facts and the meta places it.
        : <StatusLine state={i.level === 'error' ? 'error' : 'warn'}>{i.assignee
          ? <>Assigned to {i.assignee} and open for {i.first.replace(' ago', '')}: resolve it in a release, or hand it back.</>
          : <>Unassigned for {i.first.replace(' ago', '')}: assign it, or resolve it in {i.release} — it reopens by itself if it comes back later.</>}</StatusLine>
  const facts: KV[] = [
    { k: 'events 24h', v: i.events24h.toLocaleString(), mono: true }, { k: 'users', v: String(i.users), mono: true },
    { k: 'first seen', v: `${i.first} · ${state === 'regressed' ? i.resolvedIn : i.release}`, mono: true }, { k: 'last seen', v: i.last, mono: true },
    { k: 'release', v: i.release, mono: true, state: state === 'regressed' ? 'error' : undefined }, { k: 'handled', v: i.handled ? 'yes' : 'no', mono: true, state: i.handled ? undefined : 'warn' },
  ]
  // Project and environment live in the meta (`id · project · env`); the lede says only what the meta cannot.
  const lede = <Lede state={STATE_GLYPH[state]} word={word} facts={facts}><span className="font-mono">{i.culprit}</span> in <span className="font-mono">{i.fn}</span></Lede>
  // The facet holds every occurrence and hands the Ledger one page of them, so `[` `]` and the pager
  // actually move: a pager over rows that never change is a drawn control that is not wired.
  const EVENT_PAGE = 12
  const events = useMemo(() => Array.from({ length: i.events24h }, (_, n) => ({ id: `ev_${(9120 + n * 37).toString(36)}`, n })), [i.events24h])
  const matched = useMemo(() => { const needle = q.trim().toLowerCase(); return needle ? events.filter((e) => e.id.toLowerCase().includes(needle)) : events }, [events, q])
  const ago = (m: number) => (m < 60 ? `${m}m ago` : m < 1440 ? `${Math.floor(m / 60)}h ago` : `${Math.floor(m / 1440)}d ago`)
  const eventRows: LedgerRow[] = matched.slice((page - 1) * EVENT_PAGE, page * EVENT_PAGE).map((e) => ({
    id: e.id, state: 'error', onOpen: () => notify('ok', `open ${e.id}`, 'the same page with this event\'s stack, breadcrumbs and tags'),
    mobile: <><span className="block font-mono">{e.id}</span><span className="block text-[11px] text-muted-foreground">{ago(e.n * 2 + 1)} · Chrome 129 · macOS · /checkout</span></>,
    cells: [<span className="font-mono">{e.id}</span>, <span className="text-muted-foreground">{ago(e.n * 2 + 1)}</span>, <span className="font-mono text-muted-foreground">u_{(4100 + e.n * 91).toString(36)}</span>, <span className="text-muted-foreground">{['Chrome 129 · macOS', 'Safari 18 · iOS', 'Chrome 129 · Windows', 'Firefox 130 · Linux'][e.n % 4]}</span>, <span className="font-mono text-muted-foreground">/checkout</span>, <span className="font-mono">{i.release}</span>],
  }))
  return (
    <Detail title={<span className="min-w-0"><span className="font-semibold">{i.type}</span> <span className="font-normal text-muted-foreground">{i.message}</span></span>} mark={<ProjectMark name={i.project} icon={PROJECT_ICONS[i.project]} size={24} />} meta={`${i.id} · ${i.project} · ${i.env}`} status={status} lede={tab === 'overview' ? lede : undefined}
      tabs={TABS} tab={tab} onTab={(t) => { setTab(t); setQ(''); setPage(1) }}
      actions={<>
        {state !== 'resolved' && <Button size="sm" className="op-primary h-7 text-xs" onClick={() => { setState('resolved'); notify('ok', `${i.id} resolved`, 'reopens if it comes back in a later release') }}><Check /> resolve</Button>}
        {state === 'resolved' && <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => setState('unresolved')}>reopen</Button>}
        {state !== 'ignored' && <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><BellOff /> ignore</Button>} title={`Ignore ${i.type}`} description="Still counted, never raised in the verdict or notifications. Type the issue id to confirm." confirmWord={i.id} steps={['mark ignored', 'mute notifications']} onDone={() => setState('ignored')} />}
        <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'assign', 'picks a member; they get the notifications')}><User /> {i.assignee ?? 'assign'}</Button>
        <Button size="sm" variant="outline" className="h-7 text-xs" asChild><a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'opens the file in the editor', `vscode://file/${i.culprit}`) }}><Code /> open in editor</a></Button>
      </>}>

      {tab === 'overview' && (
        <Columns>
          <div>
            <Section title="Events" meta="24h · ┆ deploy · users below">
              <div className="border bg-background p-3">
                <TimeChart data={HOURLY} series={[{ key: 'events', name: 'events' }, { key: 'users', name: 'users' }]} unit="" height={150} xInterval={11} markers={[{ id: 'dep_91a', x: '20:00' }]} readoutFormat={(p) => `${p.t} · ${p.events} events · ${p.users} users`} />
              </div>
              <ChartFooter><span>events / 30 min</span><span>· the thin line is users</span></ChartFooter>
            </Section>
            <Section title="Stack trace" meta="in-app frames open · source mapped from dep_91a" action={<a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'raw event JSON', '14 KB · copied') }} className="text-xs">raw</a>}>
              <StackTrace frames={FRAMES} />
            </Section>
            <Section title="Breadcrumbs" meta="the 14s before · newest last">
              <Timeline items={CRUMBS} />
            </Section>
          </div>
          <div>
            <Section title="Latest event" meta="2m ago" action={<a href="#" onClick={(e) => { e.preventDefault(); setTab('events') }} className="text-xs">all events</a>}>
              <KeyValue compact rows={[{ k: 'event', v: 'ev_7a2k', mono: true, copy: 'ev_7a2k' }, { k: 'user', v: 'u_3f9q · jules@example.com', mono: true }, { k: 'url', v: '/checkout', mono: true }, { k: 'browser', v: 'Chrome 129 · macOS 15', mono: true }, { k: 'release', v: i.release, mono: true }, { k: 'replay', v: <Phrase onClick={() => notify('ok', 'session replay', '14s before the error, synced with the breadcrumbs')}>watch 14s</Phrase> }]} />
            </Section>
            <Section title="Tags" meta="share of 24h events" action={<a href="#" onClick={(e) => { e.preventDefault(); setTab('tags') }} className="text-xs">all tags</a>}>
              <div className="space-y-3">
                <Breakdown rows={[{ label: 'dep_91a', count: 1198 }, { label: 'dep_90e', count: 6 }]} total={1204} unit="events" limit={3} />
                <Breakdown rows={[{ label: 'Chrome', count: 720 }, { label: 'Safari', count: 361 }, { label: 'Firefox', count: 84 }, { label: 'Edge', count: 39 }]} total={1204} unit="events" limit={3} />
                <Breakdown rows={[{ label: '/checkout', count: 1190 }, { label: '/cart', count: 14 }]} total={1204} unit="events" limit={3} />
              </div>
            </Section>
            <Section title="Similar" meta="same frame, other project">
              <ol className="op-rows border bg-background text-xs"><li className="flex items-center justify-between gap-2 px-3 py-2"><span className="min-w-0 truncate"><Phrase onClick={() => go('issue:i_4502')}>TypeError</Phrase> <span className="text-muted-foreground">reading 'focus'</span></span><span className="shrink-0 text-muted-foreground">acme-crm · resolved</span></li></ol>
            </Section>
          </div>
        </Columns>
      )}

      {tab === 'events' && (
        <Ledger status={null} dense={dense}
          columns={['event', 'when', 'user', 'browser', 'url', 'release']}
          grid="minmax(6rem,1fr) minmax(64px,max-content) minmax(6rem,1fr) minmax(10rem,1.4fr) minmax(6rem,1fr) minmax(64px,max-content)"
          rows={eventRows} total={matched.length} filter={q} onFilter={(v) => { setQ(v); setPage(1) }} placeholder="filter by event id" page={{ page, pageSize: EVENT_PAGE, total: matched.length, onPage: setPage }}
          hint={q ? `${matched.length.toLocaleString()} of ${i.events24h.toLocaleString()} events match · newest first` : `${i.events24h.toLocaleString()} events in 24h · newest first`}
          footer={<span>each event is one occurrence with its own stack, breadcrumbs and tags · ⏎ opens it</span>} />
      )}

      {tab === 'tags' && (
        <div className="op-grid grid gap-6 md:grid-cols-2 xl:grid-cols-4">
          <Section title="release" meta="events"><Breakdown rows={[{ label: 'dep_91a', count: 1198 }, { label: 'dep_90e', count: 6 }]} total={1204} unit="events" /></Section>
          <Section title="browser" meta="events"><Breakdown rows={[{ label: 'Chrome', count: 720 }, { label: 'Safari', count: 361 }, { label: 'Firefox', count: 84 }, { label: 'Edge', count: 39 }]} total={1204} unit="events" /></Section>
          <Section title="url" meta="events"><Breakdown rows={[{ label: '/checkout', count: 1190 }, { label: '/cart', count: 14 }]} total={1204} unit="events" /></Section>
          <Section title="os" meta="events"><Breakdown rows={[{ label: 'macOS', count: 610 }, { label: 'iOS', count: 340 }, { label: 'Windows', count: 200 }, { label: 'Android', count: 54 }]} total={1204} unit="events" /></Section>
        </div>
      )}
    </Detail>
  )
}

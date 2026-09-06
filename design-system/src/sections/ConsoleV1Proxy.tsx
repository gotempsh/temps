// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type CSSProperties } from 'react'
import {
  Breakdown, ChartFooter, Detail, Ledger, LogLines, Num, PageState, Phrase, Picker, RangePicker, Section, Status, StatusLine, TimeChart,
  type LedgerRow, type LogLine, type Range, type State,
} from '@/components/op'
import type { Notify, Plan } from './ConsoleV1Observe'
import { useFresh } from './console-fresh'
import { PROJECT_ICONS } from './console-projects'
import { ProjectMark } from '@/components/op'

/**
 * The proxy is the hot path: every request to every project passes through
 * it. The page answers four questions in order: is it erroring, how much
 * traffic, how slow, and where does it go. One chart driven by the selected
 * tile (pick once), splits as Breakdowns rather than four multi-line charts,
 * routes as the ledger, the raw access log as a facet.
 */

const RANGES: readonly Range[] = [{ label: '1h', days: 0.05 }, { label: '6h', days: 0.25 }, { label: '24h', days: 1 }, { label: '7d', days: 7 }]
const PROJECT_OPTS = [{ value: 'all', label: 'all projects' }, { value: 'acme-storefront', label: 'acme-storefront' }, { value: 'api-gateway', label: 'api-gateway' }, { value: 'acme-crm', label: 'acme-crm' }, { value: 'docs', label: 'docs' }]
// 1h at 1-minute resolution; a burst of upstream resets at 10:44 lasting two minutes.
const T = Array.from({ length: 60 }, (_, i) => {
  const t = `${10 + Math.floor((19 + i) / 60)}:${String((19 + i) % 60).padStart(2, '0')}`
  const reset = i === 25 || i === 26
  const spike = i === 5 || i === 30 || i === 55 || i === 58 ? 40 : reset ? 48 : 0
  const total = Math.round(3 + Math.abs(Math.sin(i / 4)) * 2 + spike)
  const e5 = reset ? Math.round(total * 0.16) : 0
  const e4 = i % 9 === 0 ? 1 : 0
  return { t, total, ok: total - e4 - e5, e4, e5, err: reset ? 16.2 : 0, p50: 6 + (i % 5), p95: reset ? 210 : 40 + (i % 7) * 3 + (spike ? 30 : 0), p99: reset ? 280 : 60 + (i % 11) * 4 + (spike ? 60 : 0), project: Math.round(total * 0.24), console: Math.round(total * 0.76), other: 0 }
})
const SUM = T.reduce((a, p) => a + p.total, 0)
const E5 = T.reduce((a, p) => a + p.e5, 0)
type Route = { host: string; path: string; project: string; upstream: string; req: number; e5: number; p95: number; state: State; note?: string }
const ROUTES: Route[] = [
  { host: 'acme.sh', path: '/*', project: 'acme-storefront', upstream: 'web:3000', req: 4120, e5: 0, p95: 38, state: 'ok' },
  { host: 'api.acme.sh', path: '/v1/*', project: 'api-gateway', upstream: 'api:8080', req: 2880, e5: 91, p95: 210, state: 'error', note: '91 × 502 at 10:44 · upstream reset' },
  { host: 'crm.acme.sh', path: '/*', project: 'acme-crm', upstream: 'crm:3000', req: 610, e5: 0, p95: 52, state: 'ok' },
  { host: 'docs.acme.sh', path: '/*', project: 'docs', upstream: 'static', req: 1440, e5: 0, p95: 4, state: 'ok', note: 'static · served from disk' },
  { host: 'api.acme.sh', path: '/v1/export', project: 'api-gateway', upstream: 'api:8080', req: 22, e5: 0, p95: 1840, state: 'warn', note: 'p95 above 1s' },
  { host: 'console', path: '/api/*', project: '—', upstream: 'console:8081', req: 9310, e5: 0, p95: 11, state: 'ok', note: 'the console itself' },
]
const LOGS: LogLine[] = Array.from({ length: 60 }, (_, i) => {
  const r = ROUTES[i % ROUTES.length]
  const bad = i === 17 || i === 18 || i === 19
  return { t: `10:${String(40 + Math.floor(i / 6)).padStart(2, '0')}:${String((i * 11) % 60).padStart(2, '0')}`, level: bad ? 'error' : r.state === 'warn' && i % 5 === 0 ? 'warn' : 'info', source: r.host, msg: bad ? `GET ${r.path.replace('*', 'orders')} 502 ${r.upstream} ECONNRESET 3ms` : `GET ${r.path.replace('*', ['', 'pricing', 'docs/quickstart', 'app'][i % 4])} 200 ${r.upstream} ${r.p95 - (i % 9)}ms` }
})

const TILES = [
  { key: 'requests', label: 'requests', value: `${SUM.toLocaleString()}`, baseline: `${(SUM / 3600).toFixed(2)}/s · 24% project · 76% console`, series: [{ key: 'total', name: 'requests' }, { key: 'e5', name: '5xx', width: 1 }], unit: 'req', fmt: (p: Record<string, unknown>) => `${p.total} requests · ${p.e5} 5xx · ${p.e4} 4xx` },
  { key: 'errors', label: 'error rate', value: `${((E5 / SUM) * 100).toFixed(2)}%`, baseline: `${E5} × 5xx · all at 10:44`, series: [{ key: 'err', name: '5xx %' }], unit: '%', state: 'warn' as State, thresholds: [{ y: 1, label: '1%', state: 'warn' as const }], fmt: (p: Record<string, unknown>) => `${p.err}% 5xx` },
  { key: 'latency', label: 'p95 latency', value: '50 ms', baseline: 'p50 8 ms · p99 74 ms', series: [{ key: 'p99', name: 'p99', width: 1 }, { key: 'p95', name: 'p95', width: 1.5 }, { key: 'p50', name: 'p50', width: 2 }], unit: 'ms', fmt: (p: Record<string, unknown>) => `p50 ${p.p50} · p95 ${p.p95} · p99 ${p.p99} ms` },
  { key: 'destination', label: 'to projects', value: '24%', baseline: '76% console · 0% proxy itself', series: [{ key: 'project', name: 'project routes' }, { key: 'console', name: 'console', width: 1 }], unit: 'req', fmt: (p: Record<string, unknown>) => `${p.project} to projects · ${p.console} to the console` },
] as const
type TileKey = (typeof TILES)[number]['key']

const TABS = ['overview', 'routes', 'log'] as const
type Tab = (typeof TABS)[number]
export function ProxyScreen({ dense, plan, notify, go }: { dense: boolean; plan: Plan; notify: Notify; go: (v: string) => void }) {
  const fresh = useFresh()
  const [tab, setTab] = useState<Tab>('overview')
  const [tile, setTile] = useState<TileKey>('requests')
  const [range, setRange] = useState('1h')
  const [project, setProject] = useState('all')
  const [q, setQ] = useState('')
  const [live, setLive] = useState(true)
  const t = TILES.find((x) => x.key === tile) ?? TILES[0]
  const status = fresh
    ? <StatusLine state="idle">No requests have reached the proxy yet.</StatusLine>
    : <StatusLine state="warn" more={{ label: '+1 warning', items: [{ state: 'warn', children: <><span className="font-mono">/v1/export</span> on api.acme.sh answers in 1.8s at p95; 22 requests in the hour.</> }] }}>
      At 10:44 <Phrase onClick={() => go('issue:i_4830')}>api:8080 reset {E5} connections</Phrase>: 16% of requests got a 502 for two minutes. Since then 0 errors.
    </StatusLine>
  const routeRows: LedgerRow[] = ROUTES.filter((r) => (project === 'all' || r.project === project) && (!q || `${r.host}${r.path} ${r.upstream} ${r.project}`.toLowerCase().includes(q.toLowerCase()))).map((r) => ({
    id: `${r.host}${r.path}`, state: r.state, onOpen: () => notify('ok', `open route ${r.host}${r.path}`, 'the same page filtered to this route'),
    sort: { route: r.host + r.path, req: r.req, e5: r.e5 / r.req, p95: r.p95 },
    mobile: <><span className="block truncate font-mono">{r.host}<span className="text-muted-foreground">{r.path}</span></span><span className="block text-[11px] text-muted-foreground">{r.note ?? `${r.req.toLocaleString()} req · p95 ${r.p95} ms`}</span></>,
    cells: [
      <span className="truncate font-mono">{r.host}<span className="text-muted-foreground">{r.path}</span></span>,
      r.project === '—' ? <span className="text-muted-foreground">console</span> : <span className="flex items-center gap-1.5"><ProjectMark name={r.project} icon={PROJECT_ICONS[r.project]} /><span className="truncate">{r.project}</span></span>,
      <span className="font-mono text-muted-foreground">{r.upstream}</span>,
      <Num value={r.req} />,
      // Colour never sits on a bare number: a cell that is not fine carries the glyph with it.
      r.e5
        ? <Status state="error" label={`${((r.e5 / r.req) * 100).toFixed(2)}%`} className="w-full justify-end font-mono tabular-nums" />
        : <Num value={((r.e5 / r.req) * 100).toFixed(2)} unit="%" />,
      r.p95 > 1000
        ? <Status state="warn" label={`${r.p95} ms`} className="w-full justify-end font-mono tabular-nums" />
        : <Num value={r.p95} unit="ms" />,
      <Status state={r.state} label={r.note ?? ''} />,
    ],
  }))
  return (
    <Detail title="Proxy" meta={fresh ? 'control plane · hetzner-1 · no traffic yet' : `control plane · hetzner-1 · ${SUM.toLocaleString()} requests · ${range}`} status={status} tabs={TABS} tab={tab} onTab={(tb) => { setTab(tb); setQ('') }}
      actions={<>
        <Picker value={project} onChange={setProject} options={PROJECT_OPTS} mono={false} className="h-7 text-xs" width="220px" />
        <RangePicker ranges={RANGES} value={range} onChange={setRange} retentionDays={plan.retentionDays} retentionLabel={plan.retention} onGated={(r) => notify('warn', `${r.label} is beyond this plan's retention`, `currently ${plan.retention}`)} />
      </>}>
      {fresh && <PageState state="empty" title="No requests yet" reason="The proxy answers on :80 and :443 for every domain attached to a project. Attach a domain, or open a project's default *.temps URL, and requests appear here within a second." next={<a href="#" onClick={(e) => { e.preventDefault(); go('projects') }} className="underline underline-offset-4">projects</a>} />}
      {!fresh && tab === 'overview' && (
        <div className="space-y-6">
          <div className="op-tiles" style={{ '--tiles': 4 } as CSSProperties}>
            {TILES.map((x) => { const on = x.key === tile; return (
              <button key={x.key} type="button" aria-pressed={on} onClick={() => setTile(x.key)} className={`min-w-0 p-3 text-left transition-colors hover:bg-muted/40 ${on ? 'bg-muted/60' : ''}`}>
                <p className="op-label">{x.label}</p>
                <p className="mt-1 flex items-baseline gap-2 font-mono text-lg leading-6">{x.value}{'state' in x && x.state && <span className="text-xs"><Status state={x.state} label="one burst" /></span>}</p>
                <p className="mt-0.5 truncate text-[11px] text-muted-foreground">{x.baseline}</p>
              </button>
            ) })}
          </div>
          <div className="space-y-2">
            <div className="border bg-background p-3">
              <TimeChart data={T} series={[...t.series]} unit={t.unit} height={200} xInterval={9} markers={[{ id: 'dep_91a', x: '10:41' }]} thresholds={'thresholds' in t ? [...t.thresholds] : undefined} readoutFormat={(p) => `${p.t} · ${t.fmt(p as Record<string, unknown>)}`} />
            </div>
            <ChartFooter><span>{t.label} / minute · {range}</span><span>· ┆ deploy</span>{tile === 'latency' && <span>· thick p50, thin p99</span>}{tile === 'requests' && <span>· thin line 5xx</span>}</ChartFooter>
          </div>
          <div className="op-grid grid gap-6 md:grid-cols-2 xl:grid-cols-4">
            <Section title="Status" meta="share of requests"><Breakdown rows={[{ label: '2xx', count: SUM - E5 - 7 }, { label: '3xx', count: 0 }, { label: '4xx', count: 7 }, { label: '5xx', count: E5, state: 'error' }]} total={SUM} unit="requests" /></Section>
            <Section title="Destination" meta="who answered"><Breakdown rows={[{ label: 'console', count: Math.round(SUM * 0.76) }, { label: 'project routes', count: Math.round(SUM * 0.24) }, { label: 'proxy itself', count: 0, children: [{ label: 'ACME challenges', count: 0 }, { label: 'redirects', count: 0 }] }]} total={SUM} unit="requests" /></Section>
            <Section title="Slowest routes" meta="p95 · worst first"><Breakdown rows={[...ROUTES].sort((a, b) => b.p95 - a.p95).slice(0, 4).map((r) => ({ label: <span className="font-mono">{r.host}{r.path}</span>, key: r.host + r.path, count: r.p95, state: r.p95 > 1000 ? 'warn' : undefined, onOpen: () => setTab('routes') }))} total={Math.max(...ROUTES.map((r) => r.p95))} unit="ms" percent={false} more={{ label: 'all routes', onClick: () => setTab('routes') }} /></Section>
            <Section title="Upstreams" meta="5xx per upstream"><Breakdown rows={[{ label: 'api:8080', count: 91, state: 'error' }, { label: 'web:3000', count: 0 }, { label: 'crm:3000', count: 0 }, { label: 'console:8081', count: 0 }]} total={91} unit="5xx" percent={false} /></Section>
          </div>
        </div>
      )}
      {!fresh && tab === 'routes' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'route', key: 'route' }, 'project', 'upstream', { label: 'requests', key: 'req', numeric: true }, { label: '5xx', key: 'e5', numeric: true }, { label: 'p95', key: 'p95', numeric: true }, 'state']}
          grid="minmax(12rem,2fr) minmax(8rem,1fr) minmax(7rem,1fr) minmax(70px,max-content) minmax(60px,max-content) minmax(70px,max-content) minmax(12rem,1.6fr)"
          rows={routeRows} total={ROUTES.length} filter={q} onFilter={setQ} placeholder="filter by host, path, upstream, project"
          hint="× 5xx in the range · ◐ p95 above 1s · sorted by requests"
          footer={<span>a route is host + path prefix → upstream, as the proxy resolves it · static routes are served from disk and have no upstream</span>} />
      )}
      {!fresh && tab === 'log' && (
        <Section title="Access log" meta={live ? 'live · newest at the bottom · sampled 1 in 10 above 1k req/s' : 'paused'} action={<button type="button" className="text-xs text-muted-foreground hover:text-foreground" onClick={() => setLive((l) => !l)}>{live ? 'pause' : 'resume'}</button>}>
          <LogLines lines={LOGS} live={live} height={440} />
        </Section>
      )}
    </Detail>
  )
}

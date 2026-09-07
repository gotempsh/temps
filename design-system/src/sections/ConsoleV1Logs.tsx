// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router'
import { Bell, Box, Columns3, Container, Copy, Cpu, Download, GitBranch, Hammer, Link as LinkIcon, Rocket, ScrollText, Server, Terminal, Waypoints, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Breakdown, ChartFooter, Columns, Detail, Drop, Kbd, KbdPair, KeyValue, Ledger, Lede, Live, LogLines, Num, PageState, PageTitle,
  Phrase, Picker, ProjectMark, RangePicker, Section, Segmented, Sparkline, Status, StatusLine, TimeChart, Waterfall, worst,
  fmtAbsolute, fmtCount, fmtNum, fmtRelative,
  type BreakdownRow, type KV, type LedgerColumn, type LedgerRow, type LogLine, type Range, type Span as VizSpan, type State, type TimePoint, type TimeRange,
  Inspector, type InspectorAnchor,
} from '@/components/op'
import { checkoutTrace, type Notify, type Plan } from './ConsoleV1Observe'
import { useFresh } from './console-fresh'
import { PROJECT_ICONS } from './console-projects'
import { matches } from './ConsoleV1Admin'
import { cn } from '@/lib/utils'

/**
 * Logs: every line every application on this instance wrote, in one list,
 * with a query bar in front of it. The tool shape, not a record shape —
 * there are no tabs, because the reader does not arrive for a facet of a
 * resource, they arrive with a question ("what did billing-worker say after
 * dep_31c") and narrow one list until it answers.
 *
 * The controls are one control. The query bar owns `/` and holds the truth:
 * every facet in the aside, every scope Picker, every saved query and every
 * pattern row writes a token into it, so what narrowed the list is always
 * readable, always removable, always copyable as a string, and always in the
 * URL — a link to a log search is a link to the same lines tomorrow.
 *
 * Real shapes behind the fixtures, read from `temps/` (never edited here):
 * `log_events` (time, project_id, service, env, level, message, fields JSONB,
 * chunk_id, line_offset, deploy_id), `log_chunks` (container_id, node_name,
 * external_service_id, started_at, line_count) and `deployment_container_logs`
 * (container_name, service_name, node_id). That is why a line carries a
 * service, a node, a deployment and a chunk offset, and why "the twenty lines
 * around this one" is a real query rather than a nicety.
 */

// ── Vocabulary ───────────────────────────────────────────────────────

type Level = 'error' | 'warn' | 'info' | 'debug'
type Src = 'runtime' | 'build' | 'proxy' | 'system' | 'agent'
type Log = {
  id: string
  ts: number
  level: Level
  project: string
  env: 'production' | 'staging'
  source: Src
  service: string
  node: string
  deploy: string
  /** The line as it reads, ANSI already stripped. */
  msg: string
  /** The line as the container wrote it, escapes and all. Shown in the record. */
  raw?: string
  /** The message is a JSON object; the expansion and the record re-indent it. */
  json?: boolean
  fields?: Record<string, string | number>
  trace?: string
  request?: string
}

/** A level is a state: error ×, warn ◐, info ○. Debug is muted ink and the word, no glyph to spend. */
const LEVEL_STATE: Record<Level, State> = { error: 'error', warn: 'warn', info: 'idle', debug: 'idle' }
const SOURCE_ICON: Record<Src, typeof Terminal> = { runtime: Container, build: Hammer, proxy: Waypoints, system: Server, agent: Terminal }

const ESC = String.fromCharCode(27)
const ANSI = new RegExp(`${ESC}\\[[0-9;]*m`, 'g')
const strip = (s: string) => s.replace(ANSI, '')
const colour = (code: string, body: string) => `${ESC}[${code}m${body}${ESC}[0m`

// ── Fixtures ─────────────────────────────────────────────────────────
/* Seeded and clock-fixed: the same ~420 lines every render, so a visual
   baseline is a fact about the design and not about the hour it ran in. */

const NOW = Date.parse('2026-09-06T21:29:00Z')
const MIN = 60_000
const HOUR = 3_600_000

function seeded(seed: number) {
  let s = seed
  return () => {
    s |= 0
    s = (s + 0x6d2b79f5) | 0
    let t = Math.imul(s ^ (s >>> 15), 1 | s)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
const rnd = seeded(20260906)
const pick = <T,>(a: readonly T[]): T => a[Math.floor(rnd() * a.length)]
const int = (lo: number, hi: number) => lo + Math.floor(rnd() * (hi - lo + 1))
const id36 = (n: number) => n.toString(36).padStart(6, '0')

let seq = 0
const mk = (o: Omit<Log, 'id'>): Log => ({ id: `log_${id36(0x5f0000 + seq++ * 977)}`, ...o })

const ROUTES = ['/api/products', '/api/cart', '/checkout', '/api/orders/:id', '/healthz'] as const
const HOSTS = ['docs.acme.sh/guide', 'docs.acme.sh/cli', 'docs.acme.sh/', 'docs.acme.sh/pricing'] as const
const TRACE_IDS = ['3f9c1e7a8b2d4f60', '9a0d44c2e1f7b3a5', 'b71e0c9d5a3f2e84', 'd4e5f6a7b8c9d0e1'] as const

function gateway(ts: number): Omit<Log, 'id'> {
  const base = { ts, project: 'api-gateway', env: 'production' as const, source: 'runtime' as const, service: 'api-gateway-7c4', node: 'cp-1', deploy: 'dep_91a' }
  const r = rnd()
  const route = pick(ROUTES)
  const method = route === '/checkout' ? 'POST' : 'GET'
  const ms = int(6, 240)
  const req = `req_${id36(int(100000, 999999))}`
  if (r < 0.1) return { ...base, level: 'debug', msg: `redis GET cart:usr_${id36(int(1000, 99999))} hit in 1ms`, fields: { duration_ms: 1 } }
  if (r < 0.16) return { ...base, level: 'debug', json: true, msg: '{"event":"pool.stats","idle":7,"in_use":3,"waiters":0}' }
  if (r < 0.22) return { ...base, level: 'warn', msg: 'slow query 5.2s: SELECT * FROM orders WHERE customer_id = $1', fields: { duration_ms: 5210, route: '/api/orders/:id' }, request: req }
  if (r < 0.26) return { ...base, level: 'warn', msg: 'upstream 10.0.3.4:8080 reset the connection, retrying once', fields: { status: 502, route }, request: req, trace: pick(TRACE_IDS) }
  return { ...base, level: 'info', msg: `${method} ${route} 200 in ${ms}ms`, fields: { method, route, status: 200, duration_ms: ms }, request: req, trace: rnd() < 0.35 ? pick(TRACE_IDS) : undefined }
}

function billingQuiet(ts: number): Omit<Log, 'id'> {
  const base = { ts, project: 'billing-worker', env: 'production' as const, source: 'runtime' as const, service: 'billing-worker-2f1', node: 'worker-2', deploy: 'dep_30a' }
  const r = rnd()
  const took = int(1400, 2600)
  if (r < 0.2) return { ...base, level: 'debug', msg: 'queue poll, 0 messages, next in 5s' }
  if (r < 0.3) return { ...base, level: 'info', json: true, msg: `{"event":"invoice.generated","invoice":"inv_${id36(int(1000, 99999))}","took_ms":${took},"lines":${int(1, 9)}}` }
  return { ...base, level: 'info', msg: `invoice.generate inv_${id36(int(1000, 99999))} in ${(took / 1000).toFixed(1)}s`, fields: { duration_ms: took }, trace: rnd() < 0.3 ? 'd4e5f6a7b8c9d0e1' : undefined }
}

function storefront(ts: number): Omit<Log, 'id'> {
  const base = { ts, project: 'acme-storefront', env: 'staging' as const, source: 'build' as const, service: 'builder', node: 'cp-1', deploy: 'dep_92b' }
  const r = rnd()
  const raw = r < 0.25 ? `${colour('36', 'vite')} v6.0.3 building for production...`
    : r < 0.45 ? `${colour('32', 'ok')} ${int(400, 900)} modules transformed`
      : r < 0.55 ? `${colour('33', '(!)')} some chunks are larger than 500 kB after minification`
        : r < 0.7 ? `cache: restored node_modules from layer sha256:${id36(int(100000, 999999))}`
          : `dist/assets/index-${id36(int(1000, 99999))}.js ${int(40, 320)} kB, gzip ${int(12, 90)} kB`
  return { ...base, level: r < 0.45 ? 'info' : r < 0.55 ? 'warn' : r < 0.7 ? 'debug' : 'info', raw, msg: strip(raw) }
}

function docsProxy(ts: number): Omit<Log, 'id'> {
  const base = { ts, project: 'docs', env: 'production' as const, source: 'proxy' as const, service: 'proxy', node: 'cp-1', deploy: 'dep_12b' }
  const host = pick(HOSTS)
  const ms = int(3, 60)
  const r = rnd()
  const status = r < 0.07 ? 404 : 200
  return {
    ...base,
    level: status === 404 ? 'warn' : 'info',
    msg: `GET ${host} ${status} ${int(1, 40)} kB in ${ms}ms`,
    fields: { method: 'GET', route: `/${host.split('/').slice(1).join('/')}`, status, duration_ms: ms },
    request: `req_${id36(int(100000, 999999))}`,
  }
}

function platform(ts: number): Omit<Log, 'id'> {
  const base = { ts, project: 'api-gateway', env: 'production' as const, node: 'cp-1', deploy: 'dep_91a' }
  const r = rnd()
  if (r < 0.35) return { ...base, source: 'system', service: 'log-collector', level: 'info', msg: `chunk written, ${int(400, 2400)} lines, 41 kB compressed` }
  if (r < 0.5) return { ...base, source: 'system', service: 'acme', level: 'info', msg: 'certificate for api.acme.sh renewed, valid 89 days' }
  if (r < 0.62) return { ...base, source: 'system', service: 'scheduler', level: 'debug', msg: 'retention sweep: 0 chunks past 30d' }
  if (r < 0.8) return { ...base, source: 'agent', service: 'agent-runner', level: 'info', msg: `run_${id36(int(1000, 99999))} finished, 3 files changed, 41s` }
  return { ...base, source: 'agent', service: 'agent-runner', level: 'debug', msg: 'tool read src/checkout/AddressForm.tsx, 88 lines' }
}

/** The background: 24h up to an hour ago, and nothing red in it. */
const BACKGROUND: Log[] = Array.from({ length: 322 }, (_, i) => {
  const ts = NOW - 24 * HOUR + 2 * MIN + Math.floor(((i + rnd()) / 322) * (23 * HOUR))
  const r = rnd()
  return mk(r < 0.42 ? gateway(ts) : r < 0.62 ? billingQuiet(ts) : r < 0.78 ? storefront(ts) : r < 0.93 ? docsProxy(ts) : platform(ts))
}).filter((l) => l.ts < NOW - 65 * MIN)

/** dep_31c ships to billing-worker at 19:30 and the health check never passes again. */
const DEPLOY_AT = NOW - 90 * MIN
const BURST: Log[] = [
  mk({ ts: DEPLOY_AT, level: 'info', project: 'billing-worker', env: 'production', source: 'system', service: 'deployer', node: 'worker-2', deploy: 'dep_31c', msg: 'dep_31c is serving production, 2 of 2 replicas started' }),
  ...Array.from({ length: 52 }, (_, i) => {
    const ts = DEPLOY_AT + 5 * MIN + Math.floor((i / 52) * 85 * MIN)
    const base = { ts, project: 'billing-worker', env: 'production' as const, source: 'runtime' as const, service: 'billing-worker-2f1', node: 'worker-2', deploy: 'dep_31c' }
    return mk(i % 9 === 8
      ? { ...base, level: 'warn' as const, msg: `container restarted after 3 consecutive health check failures, restart ${Math.floor(i / 9) + 1}`, fields: { route: '/healthz' } }
      : { ...base, level: 'error' as const, msg: 'health check GET /healthz timed out after 30s', fields: { method: 'GET', route: '/healthz', status: 504, duration_ms: 30000 }, trace: 'd4e5f6a7b8c9d0e1', request: `req_${id36(int(100000, 999999))}` })
  }),
  // The other three errors of the last hour, so "38 of them from billing-worker" is a real share and not a boast.
  mk({ ts: NOW - 52 * MIN, level: 'error', project: 'docs', env: 'production', source: 'proxy', service: 'proxy', node: 'cp-1', deploy: 'dep_12b', msg: 'GET docs.acme.sh/cli 502 in 30011ms, upstream never answered', fields: { method: 'GET', route: '/cli', status: 502, duration_ms: 30011 }, request: 'req_1a9f0c' }),
  mk({ ts: NOW - 31 * MIN, level: 'error', project: 'api-gateway', env: 'production', source: 'runtime', service: 'api-gateway-7c4', node: 'cp-1', deploy: 'dep_91a', msg: "TypeError: cannot read properties of undefined (reading 'id') at src/checkout/AddressForm.tsx:88", fields: { route: '/checkout', status: 500 }, trace: '3f9c1e7a8b2d4f60', request: 'req_77c210' }),
  mk({ ts: NOW - 11 * MIN, level: 'error', project: 'acme-storefront', env: 'staging', source: 'build', service: 'builder', node: 'cp-1', deploy: 'dep_92e', raw: `${colour('31', 'error')} during build: src/checkout/AddressForm.tsx(88,20): error TS2532: object is possibly 'undefined'`, msg: strip(`${colour('31', 'error')} during build: src/checkout/AddressForm.tsx(88,20): error TS2532: object is possibly 'undefined'`) }),
]

/** Quiet company for the last hour, so the burst sits inside real traffic. */
const RECENT: Log[] = Array.from({ length: 46 }, (_, i) => {
  const ts = NOW - 62 * MIN + Math.floor((i / 46) * 61 * MIN)
  const r = rnd()
  const line = r < 0.5 ? gateway(ts) : r < 0.75 ? docsProxy(ts) : r < 0.9 ? storefront(ts) : platform(ts)
  return mk(line.level === 'error' ? { ...line, level: 'warn' } : line)
})

const LINES: Log[] = [...BACKGROUND, ...BURST, ...RECENT].sort((a, b) => b.ts - a.ts)
/** What the store is holding at this retention; the footer says it so "showing 100" is not read as "all there is". */
const TOTAL_KEPT = 41208

/** The tail the live toggle appends, read in order, so live is deterministic too. */
const TAIL: Log[] = Array.from({ length: 24 }, (_, i) =>
  mk(i % 4 === 3
    ? { ts: NOW + (i + 1) * 4000, level: 'error', project: 'billing-worker', env: 'production', source: 'runtime', service: 'billing-worker-2f1', node: 'worker-2', deploy: 'dep_31c', msg: 'health check GET /healthz timed out after 30s', fields: { method: 'GET', route: '/healthz', status: 504, duration_ms: 30000 }, trace: 'd4e5f6a7b8c9d0e1' }
    : gateway(NOW + (i + 1) * 4000)))

const PROJECTS = ['api-gateway', 'billing-worker', 'acme-storefront', 'docs'] as const

// ── The query ────────────────────────────────────────────────────────

const KEYS = ['level', 'project', 'env', 'source', 'service', 'node', 'deploy', 'trace', 'request', 'status', 'method', 'route', 'slower', 'pattern'] as const
type Key = (typeof KEYS)[number]
type Token = { k: Key; v: string }
const KEY_HELP: Record<Key, string> = {
  level: 'error · warn · info · debug',
  project: 'which application wrote it',
  env: 'production · staging',
  source: 'runtime · build · proxy · system · agent',
  service: 'the container that wrote it',
  node: 'the machine it ran on',
  deploy: 'the deployment that was live',
  trace: 'one distributed trace',
  request: 'one request id',
  status: 'HTTP status, from the structured fields',
  method: 'HTTP method, from the structured fields',
  route: 'route, from the structured fields',
  slower: 'lines whose duration is over this',
  pattern: 'one message template, from the patterns view',
}
const SLOWER: Record<string, number> = { '500ms': 500, '1s': 1000, '3s': 3000, '30s': 30000 }

/** A message with its variables removed, so a thousand lines read as one thing that happened a thousand times. */
const template = (m: string) => m
  .replace(/\b[a-z]+_[0-9a-z]{4,}\b/g, '‹id›')
  .replace(/\b[0-9a-f]{12,}\b/g, '‹id›')
  .replace(/(^|[\s(])\/[^\s,)]+/g, '$1‹path›')
  .replace(/\d+(?:\.\d+)?/g, '‹n›')

const valueOf = (l: Log, k: Key): string | undefined => {
  switch (k) {
    case 'level': return l.level
    case 'project': return l.project
    case 'env': return l.env
    case 'source': return l.source
    case 'service': return l.service
    case 'node': return l.node
    case 'deploy': return l.deploy
    case 'trace': return l.trace
    case 'request': return l.request
    case 'pattern': return template(l.msg)
    case 'status': return l.fields?.status === undefined ? undefined : String(l.fields.status)
    case 'method': return l.fields?.method === undefined ? undefined : String(l.fields.method)
    case 'route': return l.fields?.route === undefined ? undefined : String(l.fields.route)
    case 'slower': return undefined
  }
}
function hits(l: Log, t: Token) {
  if (t.k === 'slower') return Number(l.fields?.duration_ms ?? 0) > (SLOWER[t.v] ?? Number.POSITIVE_INFINITY)
  return valueOf(l, t.k) === t.v
}
/** Tokens of one key are an "or"; different keys are an "and". Two project chips mean both projects. */
function runQuery(list: Log[], tokens: Token[], text: string) {
  const byKey = new Map<Key, Token[]>()
  for (const t of tokens) byKey.set(t.k, [...(byKey.get(t.k) ?? []), t])
  const groups = [...byKey.values()]
  return list.filter((l) => groups.every((g) => g.some((t) => hits(l, t))) && matches(text, l.msg, l.service, l.project, l.deploy, l.trace, l.request))
}
const same = (a: Token, b: Token) => a.k === b.k && a.v === b.v
const tokenText = (t: Token) => `${t.k}:${t.v}`

/** The query as a string, both in the URL and on the clipboard: one grammar, one truth. */
function writeQuery(tokens: Token[], text: string) {
  return [...tokens.map((t) => `${t.k}:${t.v.includes(' ') ? `"${t.v}"` : t.v}`), ...(text ? [text] : [])].join(' ')
}
function readQuery(q: string): { tokens: Token[]; text: string } {
  const tokens: Token[] = []
  const words: string[] = []
  for (const part of q.match(/(?:[a-z]+:"[^"]*")|(?:\S+)/g) ?? []) {
    const at = part.indexOf(':')
    const k = at > 0 ? part.slice(0, at) : ''
    if ((KEYS as readonly string[]).includes(k)) tokens.push({ k: k as Key, v: part.slice(at + 1).replace(/^"|"$/g, '') })
    else words.push(part)
  }
  return { tokens, text: words.join(' ') }
}

function presetQuery(preset?: string) {
  if (!preset) return ''
  const at = preset.indexOf(':')
  const k = at > 0 ? preset.slice(0, at) : ''
  return (KEYS as readonly string[]).includes(k) ? writeQuery([{ k: k as Key, v: preset.slice(at + 1) }], '') : ''
}

/** Saved queries: the four the console ships with, plus whatever the reader names. */
type Saved = { name: string; query: string }
const BUILT_IN: Saved[] = [
  { name: 'everything', query: '' },
  { name: 'errors', query: 'level:error' },
  { name: 'deploys', query: 'source:build' },
  { name: 'slow requests', query: 'slower:1s' },
]

/** Two to four renderings of the same list, and a Segmented is what says so. */
const VIEWS = [['list', 'list'], ['patterns', 'patterns'], ['service', 'by service']] as const
type ViewId = (typeof VIEWS)[number][0]

/** Trailing cells the reader can add; the first two are on by default. */
const TRAILING = ['deployment', 'trace', 'node', 'request', 'duration', 'status'] as const
type Trailing = (typeof TRAILING)[number]
const DEFAULT_TRAILING: Trailing[] = ['deployment', 'trace']
const TRAILING_GRID: Record<Trailing, string> = {
  deployment: 'minmax(68px,max-content)', trace: 'minmax(88px,max-content)', node: 'minmax(64px,max-content)',
  request: 'minmax(88px,max-content)', duration: 'minmax(64px,max-content)', status: 'minmax(48px,max-content)',
}

const RANGES: readonly Range[] = [{ label: '1h', days: 0.05 }, { label: '24h', days: 1 }, { label: '7d', days: 7 }, { label: '30d', days: 30 }, { label: '90d', days: 90 }]
const RANGE_MS: Record<string, number> = { '1h': HOUR, '24h': 24 * HOUR, '7d': 7 * 24 * HOUR, '30d': 30 * 24 * HOUR, '90d': 90 * 24 * HOUR }
const ZONE = 'UTC'

// ── Buckets ──────────────────────────────────────────────────────────

const BUCKET_MS = 30 * MIN
/* Buckets are aligned to the half hour, not to "now": a bucket labelled 20:30 has to start at 20:30,
   or the deploy markers land beside the rise they caused instead of on it. */
const BUCKET_END = Math.floor(NOW / BUCKET_MS) * BUCKET_MS
const BUCKETS = Array.from({ length: 48 }, (_, i) => BUCKET_END - (47 - i) * BUCKET_MS)
const bucketLabel = (ts: number) => {
  const d = new Date(ts)
  return `${String(d.getUTCHours()).padStart(2, '0')}:${d.getUTCMinutes() >= 30 ? '30' : '00'}`
}
const LABELS = BUCKETS.map(bucketLabel)
const bucketIndex = (ts: number) => Math.min(47, Math.max(0, Math.floor((ts - BUCKETS[0]) / BUCKET_MS)))

// ── Screen ───────────────────────────────────────────────────────────

export function LogsScreen({ go, dense, notify, plan, preset }: { go: (v: string) => void; dense: boolean; notify: Notify; plan?: Plan; preset?: string }) {
  const fresh = useFresh()
  const [params, setParams] = useSearchParams()
  const retentionDays = plan?.retentionDays ?? 30
  const retentionLabel = plan?.retention ?? '30d'

  // The query and the chosen columns live in the URL beside `p=`, so a log search is a link.
  // A preset arrives as `logs:<key>:<value>` from another screen ("open in Logs" on a deployment);
  // it is written through the same grammar the URL uses, so a value with spaces survives the trip.
  const q = params.get('q') ?? presetQuery(preset)
  const { tokens, text } = useMemo(() => readQuery(q), [q])
  const trailing = useMemo<Trailing[]>(() => {
    const raw = params.get('cols')
    if (raw === null) return DEFAULT_TRAILING
    return raw.split(',').filter((c): c is Trailing => (TRAILING as readonly string[]).includes(c))
  }, [params])
  const view = (VIEWS.find(([v]) => v === params.get('lv'))?.[0] ?? 'list') as ViewId

  const patch = useCallback((next: Record<string, string | null>) => {
    const p = new URLSearchParams(params)
    for (const [k, v] of Object.entries(next)) { if (v === null) p.delete(k); else p.set(k, v) }
    setParams(p, { replace: true })
  }, [params, setParams])
  const setQuery = useCallback((ts: Token[], txt: string) => { patch({ q: writeQuery(ts, txt) || null }); setPage(1) }, [patch])
  const add = useCallback((t: Token) => { if (!tokens.some((x) => same(x, t))) setQuery([...tokens, t], text) }, [tokens, text, setQuery])
  const drop = (t: Token) => setQuery(tokens.filter((x) => !same(x, t)), text)
  const replace = (k: Key, v: string | null) => setQuery([...tokens.filter((x) => x.k !== k), ...(v ? [{ k, v }] : [])], text)

  const [range, setRange] = useState('24h')
  const [win, setWin] = useState({ from: '', to: '' })
  const [window_, setWindow] = useState<TimeRange | null>(null)
  const [live, setLive] = useState(false)
  const [tail, setTail] = useState<Log[]>([])
  const [held, setHeld] = useState<Log[]>([])
  const [expanded, setExpanded] = useState<string | null>(null)
  /* The row read beside the list. `⏎` opens it, `j`/`k` keep moving the ledger's
     cursor and the panel follows; `log:<id>` stays the deep link and the panel's
     `open` is how you get there. */
  const [inspect, setInspect] = useState<string | null>(null)
  const [facetsOpen, setFacetsOpen] = useState(false)
  const [facetQ, setFacetQ] = useState('')
  const [colsOpen, setColsOpen] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)
  const [saveOpen, setSaveOpen] = useState(false)
  const [saveName, setSaveName] = useState('')
  const [saved, setSaved] = useState<Saved[]>(BUILT_IN)
  const [wide, setWide] = useState(true)
  const [page, setPage] = useState(1)
  const [suggestOpen, setSuggestOpen] = useState(false)
  const [draft, setDraft] = useState('')
  const [sugCursor, setSugCursor] = useState(0)

  const inputRef = useRef<HTMLInputElement>(null)
  const barRef = useRef<HTMLDivElement>(null)
  const facetBtn = useRef<HTMLButtonElement>(null)
  const colsBtn = useRef<HTMLButtonElement>(null)
  const exportBtn = useRef<HTMLButtonElement>(null)
  const saveBtn = useRef<HTMLButtonElement>(null)

  // The aside is a `Columns` aside from xl; below that the same facets are a Drop, so they are never both
  // on the page at once and a phone never scrolls sideways to reach them.
  useEffect(() => {
    const mq = window.matchMedia('(min-width: 1280px)')
    const on = () => setWide(mq.matches)
    on()
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [])

  const all = useMemo(() => [...tail, ...LINES], [tail])
  const from = range === 'custom' && win.from ? Date.parse(`${win.from}:00Z`) : NOW - (RANGE_MS[range] ?? 24 * HOUR)
  const to = range === 'custom' && win.to ? Date.parse(`${win.to}:00Z`) : NOW + 10 * MIN
  const inRange = useMemo(() => (fresh ? [] : all.filter((l) => l.ts >= from && l.ts <= to)), [all, from, to, fresh])
  const inWindow = useMemo(() => (
    window_ ? inRange.filter((l) => { const i = bucketIndex(l.ts); return i >= LABELS.indexOf(window_.from) && i <= LABELS.indexOf(window_.to) }) : inRange
  ), [inRange, window_])
  const list = useMemo(() => runQuery(inWindow, tokens, text), [inWindow, tokens, text])
  /* A narrowed query can take the inspected line out of the list; the panel then
     closes rather than showing a line the query says is not here. */
  const inspectLine = inspect ? list.find((l) => l.id === inspect) ?? null : null
  /* The panel takes the aside's place at xl — one 520px column of reading beside
     the list, not two — so the facets go back behind their button, where the
     narrower widths already keep them. */
  const asideOpen = wide && !inspectLine

  // The verdict is about the scope, never about the query: narrowing the list cannot change what is wrong.
  const lastHour = inRange.filter((l) => l.ts >= NOW - HOUR && l.level === 'error')
  const burst = lastHour.filter((l) => l.project === 'billing-worker')

  // ── The scope, read as a sentence ──
  const projectTokens = tokens.filter((t) => t.k === 'project')
  const envToken = tokens.find((t) => t.k === 'env')
  const sourceToken = tokens.find((t) => t.k === 'source')
  const projectPhrase = projectTokens.length === 0 ? 'all projects' : `in ${projectTokens.map((t) => t.v).join(', ')}`
  const rangeLabel = range === 'custom' && win.from && win.to ? `${win.from.replace('T', ' ')} → ${win.to.replace('T', ' ')} ${ZONE}` : range
  const scope = [projectPhrase, envToken?.v ?? 'all environments', ...(sourceToken ? [sourceToken.v] : []), rangeLabel].join(' · ')

  // ── Chart ──
  /* The chart answers the query, not the window: a facet click narrows the plot and the list together.
     The chart selection is deliberately not applied here — a plot that redrew itself from its own
     selection would leave the reader no way back. */
  const points: TimePoint[] = useMemo(() => {
    const rows = LABELS.map((t) => ({ t, error: 0, warn: 0, info: 0, debug: 0 }))
    for (const l of runQuery(inRange, tokens, text)) rows[bucketIndex(l.ts)][l.level] += 1
    return rows as unknown as TimePoint[]
  }, [inRange, tokens, text])

  // ── Facets ──
  const countsOf = useCallback((k: Key) => {
    const m = new Map<string, { n: number; errors: number }>()
    for (const l of inWindow) {
      const v = valueOf(l, k)
      if (!v) continue
      const cur = m.get(v) ?? { n: 0, errors: 0 }
      m.set(v, { n: cur.n + 1, errors: cur.errors + (l.level === 'error' ? 1 : 0) })
    }
    return [...m.entries()].sort((a, b) => b[1].n - a[1].n)
  }, [inWindow])

  const errorBar = (errors: number, n: number) => (
    <span aria-hidden title={`${fmtNum(errors)} of ${fmtNum(n)} are errors`} className="ms-2 inline-block h-1 w-8 shrink-0 bg-muted align-middle">
      <span className="block h-full bg-foreground" style={{ width: `${n ? (errors / n) * 100 : 0}%` }} />
    </span>
  )
  const facetIcon = (k: Key, v: string) => {
    if (k === 'project') return <ProjectMark name={v} icon={PROJECT_ICONS[v]} />
    if (k === 'source') { const I = SOURCE_ICON[v as Src]; return <I aria-hidden /> }
    if (k === 'node') return <Cpu aria-hidden />
    if (k === 'service') return <Container aria-hidden />
    if (k === 'deploy') return <Rocket aria-hidden />
    if (k === 'env') return <GitBranch aria-hidden />
    return undefined
  }
  const facetRows = useCallback((k: Key): BreakdownRow[] => countsOf(k)
    .filter(([v]) => matches(facetQ, v))
    .map(([v, c]) => ({
      key: v, count: c.n, state: k === 'level' ? LEVEL_STATE[v as Level] : undefined, icon: facetIcon(k, v),
      label: <span className="inline-flex min-w-0 items-center"><span className="min-w-0 truncate">{v}</span>{c.errors > 0 && errorBar(c.errors, c.n)}</span>,
      onOpen: () => add({ k, v }),
    })), [countsOf, facetQ, add])

  const fieldsSeen = useMemo<BreakdownRow[]>(() => {
    const group = (k: Key) => {
      const rows = countsOf(k)
      return { count: rows.reduce((a, [, c]) => a + c.n, 0), children: rows.slice(0, 6).map(([v, c]) => ({ key: `${k}:${v}`, label: v, count: c.n, onOpen: () => add({ k, v }) })) }
    }
    const durations = inWindow.filter((l) => l.fields?.duration_ms !== undefined)
    return [
      { key: 'status', label: 'status', ...group('status') },
      { key: 'method', label: 'method', ...group('method') },
      { key: 'route', label: 'route', ...group('route') },
      { key: 'duration', label: 'duration', count: durations.length, children: Object.keys(SLOWER).map((v) => ({ key: `slower:${v}`, label: `over ${v}`, count: durations.filter((l) => Number(l.fields!.duration_ms) > SLOWER[v]).length, onOpen: () => add({ k: 'slower', v }) })) },
    ].filter((r) => matches(facetQ, r.label) || r.children.some((c) => matches(facetQ, String(c.label))))
  }, [countsOf, inWindow, facetQ, add])

  const facetBlock = (title: string, k: Key, meta?: string, limit = 5) => {
    const rows = facetRows(k)
    if (!rows.length) return null
    return <Section title={title} meta={meta}><Breakdown rows={rows} total={inWindow.length} unit="lines" limit={limit} /></Section>
  }
  const facets = (
    <div className="space-y-0">
      <Section title="Facets" meta="a row adds a token">
        <input value={facetQ} onChange={(e) => setFacetQ(e.target.value)} placeholder="filter facets" aria-label="filter facets"
          className="h-7 w-full border bg-background px-2 font-mono text-xs outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring" />
      </Section>
      {facetBlock('Level', 'level', 'the bar is the error share', 4)}
      {facetBlock('Project', 'project', undefined, 6)}
      {facetBlock('Environment', 'env', undefined, 4)}
      {facetBlock('Source', 'source', 'where the line came from', 5)}
      {facetBlock('Service', 'service', 'the container that wrote it', 6)}
      {facetBlock('Node', 'node', undefined, 4)}
      {facetBlock('Deployment', 'deploy', 'live when the line was written', 5)}
      {fieldsSeen.length > 0 && <Section title="Fields seen" meta="structured attributes"><Breakdown rows={fieldsSeen} total={inWindow.length} unit="lines" percent={false} limit={4} /></Section>}
    </div>
  )

  // ── Live tail ──
  const pinned = useRef(true)
  useEffect(() => {
    /* The list is newest first, so "pinned" means the reader is standing where the new lines land.
       Scroll away to read something older and the tail holds instead of moving the ground under them. */
    const onScroll = () => { pinned.current = window.scrollY < 240 }
    onScroll()
    window.addEventListener('scroll', onScroll, { passive: true })
    return () => window.removeEventListener('scroll', onScroll)
  }, [])
  const cursorAt = useRef(0)
  useEffect(() => {
    if (!live) return
    const t = window.setInterval(() => {
      const next = TAIL[cursorAt.current % TAIL.length]
      cursorAt.current += 1
      if (pinned.current) setTail((prev) => [next, ...prev])
      else setHeld((prev) => [next, ...prev])
    }, 2000)
    return () => window.clearInterval(t)
  }, [live])
  const resume = () => { setTail((prev) => [...held, ...prev]); setHeld([]); pinned.current = true; window.scrollTo({ top: 0 }) }

  // ── Rows ──
  const pageRows = useMemo(() => list.slice((page - 1) * 100, page * 100), [list, page])

  const trailingCell = (l: Log, c: Trailing) => {
    if (c === 'deployment') return <span className="font-mono text-muted-foreground">{l.deploy}</span>
    if (c === 'node') return <span className="font-mono text-muted-foreground">{l.node}</span>
    /* A row that carries a correlation says so with the link glyph, trace and
       request alike. These are marks, not controls: the row itself is
       `role="button"`, and a button inside a button is `nested-interactive`.
       Both are reachable without them — `t` opens the trace under the cursor,
       ⏎ opens the Inspector, whose Trace and Request sections carry the real
       actions, and the query bar takes `trace:` and `request:` directly. */
    if (c === 'request') return l.request
      ? <span title={`${l.request} · ⏎ then Request, or type request:${l.request}`} className="flex items-center gap-1 font-mono text-muted-foreground"><LinkIcon aria-hidden className="h-3.5 w-3.5" />{l.request}</span>
      : <Num value={null} />
    if (c === 'duration') return l.fields?.duration_ms === undefined ? <Num value={null} /> : <Num value={Number(l.fields.duration_ms)} unit="ms" />
    if (c === 'status') return l.fields?.status === undefined ? <Num value={null} /> : <Num value={Number(l.fields.status)} />
    return l.trace
      ? <span title={`trace ${l.trace} · press t to open it`} className="flex items-center gap-1 font-mono text-muted-foreground"><LinkIcon aria-hidden className="h-3.5 w-3.5" />{l.trace.slice(0, 8)}</span>
      : <Num value={null} />
  }

  const logRows: LedgerRow[] = pageRows.map((l) => ({
    id: l.id, state: LEVEL_STATE[l.level], onOpen: () => setInspect(l.id),
    icon: (() => { const I = SOURCE_ICON[l.source]; return <I aria-hidden /> })(),
    mobile: <>
      <span className="flex min-w-0 items-center gap-2 text-[11px]">
        {l.level === 'debug' ? <span className="shrink-0 font-mono text-muted-foreground">debug</span> : <Status state={LEVEL_STATE[l.level]} label={l.level} className="shrink-0" />}
        <span className="min-w-0 truncate font-mono text-muted-foreground">{l.service}</span>
        <span className="ms-auto shrink-0 font-mono text-muted-foreground" title={fmtAbsolute(l.ts, { tz: ZONE, seconds: true })}>{fmtRelative(l.ts, NOW)}</span>
      </span>
      <span className="mt-0.5 block truncate font-mono">{l.msg}</span>
    </>,
    cells: [
      <span className="font-mono tabular-nums text-muted-foreground" title={fmtAbsolute(l.ts, { tz: ZONE, seconds: true })}>{fmtRelative(l.ts, NOW)}</span>,
      l.level === 'debug' ? <span className="font-mono text-muted-foreground">debug</span> : <Status state={LEVEL_STATE[l.level]} label={l.level} />,
      <span className="flex min-w-0 items-center gap-1.5"><ProjectMark name={l.project} icon={PROJECT_ICONS[l.project]} /><span className="min-w-0 truncate font-mono text-muted-foreground">{l.service}</span></span>,
      <span className={cn('min-w-0 truncate font-mono', l.level === 'debug' && 'text-muted-foreground')}>{l.msg}</span>,
      ...trailing.map((c) => trailingCell(l, c)),
    ],
  }))

  // Patterns: the same query, grouped by message template. One row is a thing that happened N times.
  const patterns = useMemo(() => {
    const m = new Map<string, Log[]>()
    for (const l of list) { const t = template(l.msg); m.set(t, [...(m.get(t) ?? []), l]) }
    return [...m.entries()].map(([tpl, ls]) => ({
      tpl, n: ls.length,
      state: worst(ls.map((l) => LEVEL_STATE[l.level])),
      level: ls.some((l) => l.level === 'error') ? 'error' : ls.some((l) => l.level === 'warn') ? 'warn' : ls[0].level,
      first: Math.min(...ls.map((l) => l.ts)),
      last: Math.max(...ls.map((l) => l.ts)),
      spark: (() => { const b = new Array(48).fill(0) as number[]; for (const l of ls) b[bucketIndex(l.ts)] += 1; return b })(),
    })).sort((a, b) => b.n - a.n)
  }, [list])
  const patternMax = Math.max(1, ...patterns.map((p) => p.n))
  const patternRows: LedgerRow[] = patterns.slice(0, 100).map((p) => ({
    id: p.tpl, state: p.state, onOpen: () => { add({ k: 'pattern', v: p.tpl }); patch({ lv: 'list' }) },
    mobile: <>
      <span className="flex items-center gap-2 text-[11px]"><Status state={p.state} label={p.level} className="shrink-0" /><span className="ms-auto shrink-0 font-mono">{fmtNum(p.n)} lines</span></span>
      <span className="mt-0.5 block truncate font-mono">{p.tpl}</span>
    </>,
    cells: [
      <span className="flex min-w-0 items-center gap-2"><Status state={p.state} label={p.level} className="shrink-0" /><span className="min-w-0 truncate font-mono">{p.tpl}</span></span>,
      <Num value={p.n} />,
      <span className="flex items-center gap-2"><span aria-hidden className="h-1.5 w-full bg-muted"><span className="block h-full bg-foreground" style={{ width: `${(p.n / patternMax) * 100}%` }} /></span></span>,
      <span className="font-mono text-muted-foreground" title={fmtAbsolute(p.first, { tz: ZONE, seconds: true })}>{fmtRelative(p.first, NOW)}</span>,
      <span className="font-mono text-muted-foreground" title={fmtAbsolute(p.last, { tz: ZONE, seconds: true })}>{fmtRelative(p.last, NOW)}</span>,
      <span className="block w-full"><Sparkline points={p.spark} state={p.level === 'error' ? 'error' : undefined} /></span>,
    ],
  }))

  // By service: the same query as a ranked list, which is a Breakdown that kept the keyboard.
  const services = useMemo(() => countsOf('service').map(([v, c]) => ({ v, ...c })), [countsOf])
  const serviceMax = Math.max(1, ...services.map((s) => s.n))
  const serviceRows: LedgerRow[] = services.map((s) => ({
    id: s.v, state: s.errors > 0 ? 'error' : 'ok', onOpen: () => { add({ k: 'service', v: s.v }); patch({ lv: 'list' }) },
    icon: <Container aria-hidden />,
    mobile: <><span className="block truncate font-mono">{s.v}</span><span className="block text-[11px] text-muted-foreground">{fmtNum(s.n)} lines · {fmtNum(s.errors)} errors</span></>,
    cells: [
      <span className="min-w-0 truncate font-mono">{s.v}</span>,
      <Num value={s.n} />,
      s.errors ? <Status state="error" label={fmtNum(s.errors)} /> : <Num value={0} />,
      <span className="flex items-center gap-2"><span aria-hidden className="h-1.5 w-full bg-muted"><span className="block h-full bg-foreground" style={{ width: `${(s.n / serviceMax) * 100}%` }} /></span></span>,
    ],
  }))

  const openLine = list.find((l) => l.id === expanded)

  // ── Keyboard: every badge drawn on this page is handled here ──
  /* The Ledger's cursor IS the focus, so the row under the cursor is the focused
     `.op-row`. With the panel open focus may have Tabbed into it; the panel then
     remembers which row it is showing, which is the same row. */
  const cursorRow = useCallback(() => {
    const el = (document.activeElement as HTMLElement | null)?.closest('.op-row') as HTMLElement | null
    const id = el?.id?.startsWith('row-') ? el.id.slice(4) : inspect ?? pageRows[0]?.id
    return id ? pageRows.find((l) => l.id === id) : undefined
  }, [pageRows, inspect])
  const expandFocused = useCallback(() => {
    if (view !== 'list') return
    const rowId = cursorRow()?.id
    if (rowId) setExpanded((prev) => (prev === rowId ? null : rowId))
  }, [cursorRow, view])
  /** `t` opens the trace of the row under the cursor, and says so when the line has none. */
  const openTrace = useCallback(() => {
    if (view !== 'list') return
    const l = cursorRow()
    if (!l) return
    if (l.trace) go(`trace:${l.trace}`)
    else notify('warn', 'no trace on this line', `${l.service} did not stamp a trace id · the SDK adds one when tracing is on`)
  }, [cursorRow, view, go, notify])
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || e.metaKey || e.ctrlKey) return
      if (e.key === '/') { e.preventDefault(); inputRef.current?.focus() }
      else if (e.key === 'e') { e.preventDefault(); expandFocused() }
      else if (e.key === 't') { e.preventDefault(); openTrace() }
      else if (e.key === ' ') { e.preventDefault(); setLive((v) => !v) }
      // With the panel open `esc` is the panel's: it closes and hands focus back to the row.
      else if (e.key === 'Escape' && !inspect) { setExpanded(null); setFacetsOpen(false); setColsOpen(false); setExportOpen(false); setSaveOpen(false) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [expandFocused, openTrace, inspect])
  /* The panel follows the ledger's cursor: the cursor is the focus, so a row taking
     focus is the cursor moving. The functional update keeps a closing panel closed —
     `esc` returns focus to the row it came from, and that focus must not reopen it. */
  useEffect(() => {
    if (!inspect) return
    const onFocusIn = (e: FocusEvent) => {
      const el = (e.target as HTMLElement | null)?.closest('.op-row') as HTMLElement | null
      if (el?.id?.startsWith('row-')) setInspect((prev) => (prev ? el.id.slice(4) : prev))
    }
    window.addEventListener('focusin', onFocusIn)
    return () => window.removeEventListener('focusin', onFocusIn)
  }, [inspect])

  // ── Suggestions ──
  const [sugKey, sugPrefix] = draft.includes(':') ? [draft.slice(0, draft.indexOf(':')), draft.slice(draft.indexOf(':') + 1)] : ['', draft]
  const suggestions: { token?: Token; label: string; meta: string }[] = useMemo(() => {
    if ((KEYS as readonly string[]).includes(sugKey)) {
      const k = sugKey as Key
      if (k === 'slower') return Object.keys(SLOWER).filter((v) => v.startsWith(sugPrefix)).map((v) => ({ token: { k, v }, label: `${k}:${v}`, meta: 'duration over' }))
      return countsOf(k).filter(([v]) => matches(sugPrefix, v)).slice(0, 8).map(([v, c]) => ({ token: { k, v }, label: `${k}:${v}`, meta: `${fmtNum(c.n)} lines` }))
    }
    return KEYS.filter((k) => k.includes(sugPrefix.toLowerCase())).slice(0, 8).map((k) => ({ label: `${k}:`, meta: KEY_HELP[k] }))
  }, [sugKey, sugPrefix, countsOf])
  const accept = (i: number) => {
    const s = suggestions[i]
    if (!s) { setQuery(tokens, draft.trim()); setDraft(''); setSuggestOpen(false); return }
    if (s.token) { add(s.token); setDraft(''); setSuggestOpen(false) }
    else { setDraft(s.label); setSugCursor(0) }
  }

  const status = fresh
    ? <StatusLine state="idle">No lines yet. Nothing on this instance is shipping logs, so there is nothing to search.</StatusLine>
    : lastHour.length === 0
      ? <StatusLine state="ok">Nothing to do: no errors in the last hour across {fmtCount(PROJECTS.length, 'project')}. The busiest writer is the proxy access log.</StatusLine>
      : <StatusLine state="error" more={{ label: '+1 warning', items: [{ state: 'warn', children: <>The <Phrase onClick={() => add({ k: 'slower', v: '1s' })}>orders query</Phrase> has been over 5s on api-gateway all day. Slow, not failing.</> }] }}>
        <Phrase onClick={() => add({ k: 'level', v: 'error' })}>{fmtCount(lastHour.length, 'error')} in the last hour</Phrase>, {fmtNum(burst.length)} from <Phrase onClick={() => setQuery([{ k: 'project', v: 'billing-worker' }, { k: 'level', v: 'error' }], '')}>billing-worker</Phrase> since dep_31c: the health check has not passed since it shipped. <Phrase onClick={() => go('billing-worker')}>Open billing-worker</Phrase>.
      </StatusLine>

  const chart = (
    <Section title="Volume by level" meta="30 min buckets · drag to narrow the list">
      <div className="space-y-2">
        <TimeChart
          data={points} height={150} unit="lines" xInterval={11}
          series={[
            { key: 'error', name: 'error', stroke: 'solid', weight: 'regular', state: 'error' },
            { key: 'warn', name: 'warn', stroke: 'dashed', weight: 'thin' },
            { key: 'info', name: 'info', stroke: 'solid', weight: 'thin' },
            { key: 'debug', name: 'debug', stroke: 'dotted', weight: 'thin' },
          ]}
          markers={[{ id: 'dep_31c', x: bucketLabel(DEPLOY_AT), note: 'billing-worker' }, { id: 'dep_91a', x: '20:30', note: 'api-gateway' }]}
          selection={window_} onSelect={(w) => { setWindow(w); setPage(1) }}
          onOpen={(dep) => go(`deploy:${dep}`)}
          readoutFormat={(p) => `${p.t} ${ZONE} · ${p.error} error · ${p.warn} warn · ${p.info} info · ${p.debug} debug`}
          title="log volume by level" range={`last ${rangeLabel}`}
          verdict={`errors begin at ${bucketLabel(DEPLOY_AT)}, when dep_31c shipped to billing-worker, and do not stop`}
        />
        <ChartFooter><span>showing {rangeLabel}</span><span>· times are {ZONE}</span><span>· retention {retentionLabel}</span><span>· ┆ deploy</span><span>· a selection narrows the list below</span></ChartFooter>
      </div>
    </Section>
  )

  const queryBar = (
    <div className="space-y-2">
      <div ref={barRef} className="relative flex min-w-0 flex-wrap items-center gap-1 border px-2 py-1">
        <ScrollText aria-hidden className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
        {tokens.map((t) => (
          <span key={tokenText(t)} className="inline-flex h-5 shrink-0 items-center gap-1 border px-1.5 font-mono text-[11px]">
            {tokenText(t).length > 44 ? `${tokenText(t).slice(0, 44)}…` : tokenText(t)}
            <button type="button" aria-label={`remove ${tokenText(t)}`} onClick={() => drop(t)} className="text-muted-foreground hover:text-foreground"><X aria-hidden className="h-3 w-3" /></button>
          </span>
        ))}
        {text && (
          <span className="inline-flex h-5 shrink-0 items-center gap-1 border px-1.5 font-mono text-[11px]">
            {`"${text}"`}
            <button type="button" aria-label="remove the text search" onClick={() => setQuery(tokens, '')} className="text-muted-foreground hover:text-foreground"><X aria-hidden className="h-3 w-3" /></button>
          </span>
        )}
        <input
          ref={inputRef} value={draft} aria-label="search the logs"
          placeholder={tokens.length || text ? 'and…' : 'level:error service:billing-worker, or any words'}
          onChange={(e) => { setDraft(e.target.value); setSuggestOpen(true); setSugCursor(0) }}
          onFocus={() => setSuggestOpen(true)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') { e.preventDefault(); setSugCursor((c) => Math.min(suggestions.length - 1, c + 1)) }
            else if (e.key === 'ArrowUp') { e.preventDefault(); setSugCursor((c) => Math.max(0, c - 1)) }
            else if (e.key === 'Enter') { e.preventDefault(); accept(draft.includes(':') || !draft.trim() ? sugCursor : -1) }
            else if (e.key === 'Escape') { setSuggestOpen(false); inputRef.current?.blur() }
            else if (e.key === 'Backspace' && !draft && tokens.length) drop(tokens[tokens.length - 1])
          }}
          className="h-6 min-w-32 flex-1 bg-transparent font-mono text-xs outline-none placeholder:text-muted-foreground"
        />
        <Kbd keys="/" className="pointer-events-none shrink-0 opacity-60" />
        <Drop anchor={barRef} open={suggestOpen && suggestions.length > 0} width={460} label="query suggestions" className="start-0 end-auto top-10">
          <ol className="op-rows max-h-64 overflow-y-auto text-xs">
            {suggestions.map((s, i) => (
              <li key={s.label}>
                <button type="button" onMouseDown={(e) => { e.preventDefault(); accept(i) }}
                  className={cn('flex w-full items-center gap-3 px-3 py-1.5 text-start', i === sugCursor && 'bg-muted')}>
                  <span className="min-w-0 truncate font-mono">{s.label}</span>
                  <span className="ms-auto shrink-0 font-mono text-[11px] text-muted-foreground">{s.meta}</span>
                </button>
              </li>
            ))}
          </ol>
          <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground"><Kbd keys="⏎" /> add · <Kbd keys="esc" /> close · plain words search the message</p>
        </Drop>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Picker label="projects" width="260px" mono className="w-auto max-w-56"
          value={projectTokens.length === 1 ? projectTokens[0].v : projectTokens.length ? 'many' : 'all'}
          onChange={(v) => setQuery([...tokens.filter((t) => t.k !== 'project'), ...(v === 'all' || v === 'many' ? [] : [{ k: 'project' as Key, v }])], text)}
          options={[
            ...(projectTokens.length > 1 ? [{ value: 'many', label: projectPhrase, meta: 'from the facets' }] : []),
            { value: 'all', label: 'all projects', meta: fmtCount(PROJECTS.length, 'project') },
            ...PROJECTS.map((p) => ({ value: p, label: p, icon: <ProjectMark name={p} icon={PROJECT_ICONS[p]} />, meta: `${fmtNum(inRange.filter((l) => l.project === p).length)} lines` })),
          ]} />
        <Picker label="environment" width="220px" mono className="w-auto max-w-48" value={envToken?.v ?? 'all'} onChange={(v) => replace('env', v === 'all' ? null : v)}
          options={[{ value: 'all', label: 'all environments' }, { value: 'production', label: 'production' }, { value: 'staging', label: 'staging' }]} />
        <Picker label="source" width="240px" mono className="w-auto max-w-44" value={sourceToken?.v ?? 'all'} onChange={(v) => replace('source', v === 'all' ? null : v)}
          options={[{ value: 'all', label: 'all sources' }, ...(['runtime', 'build', 'proxy', 'system', 'agent'] as const).map((s) => ({ value: s, label: s, icon: (() => { const I = SOURCE_ICON[s]; return <I aria-hidden /> })() }))]} />
        <Picker label="saved query" width="280px" mono className="w-auto max-w-56"
          value={saved.find((s) => s.query === q)?.name ?? 'this query'}
          onChange={(name) => { const s = saved.find((x) => x.name === name); if (s) { const r = readQuery(s.query); setQuery(r.tokens, r.text) } }}
          options={[...(saved.some((s) => s.query === q) ? [] : [{ value: 'this query', label: 'this query', meta: 'not saved' }]), ...saved.map((s) => ({ value: s.name, label: s.name, meta: s.query || 'no tokens' }))]} />
        <RangePicker
          ranges={RANGES} value={range} onChange={(r) => { setRange(r); setWindow(null); setPage(1) }}
          retentionDays={retentionDays} retentionLabel={retentionLabel}
          onGated={(r) => notify('warn', `${r.label} is past this plan's retention`, `logs are kept ${retentionLabel}; older lines were deleted, not hidden`)}
          custom={{ from: win.from, to: win.to, zone: ZONE, onChange: (f, t) => { setWin({ from: f, to: t }); setRange('custom'); setPage(1) } }} />
        <span className="font-mono text-[11px] text-muted-foreground">times are {ZONE}</span>
        {/* Live pins the newest line at the top of the list until the reader scrolls away from it; then it holds and counts. */}
        <Live every="2s" paused={!live} onToggle={() => setLive((v) => !v)} />
        {!asideOpen && (
          <div className="relative">
            <button ref={facetBtn} type="button" onClick={() => setFacetsOpen((o) => !o)} aria-expanded={facetsOpen} className="inline-flex h-7 items-center gap-1.5 border px-2 text-xs hover:bg-muted">
              <Box aria-hidden className="h-3.5 w-3.5" /> facets
            </button>
            <Drop anchor={facetBtn} open={facetsOpen} width={420} label="facets">
              <div className="max-h-[70vh] overflow-y-auto p-3">{facets}</div>
            </Drop>
          </div>
        )}
        {(tokens.length > 0 || text) && (
          <button type="button" onClick={() => setQuery([], '')} className="text-[11px] text-muted-foreground underline underline-offset-4 hover:text-foreground">clear the query</button>
        )}
      </div>
    </div>
  )

  const hint = (
    <span className="flex flex-wrap items-center gap-x-2">
      {held.length > 0 && <button type="button" onClick={resume} className="font-mono text-[11px] underline underline-offset-4 hover:text-foreground">paused · {fmtCount(held.length, 'new line')} · resume</button>}
      {window_ && <span>{fmtCount(list.length, 'line')} between {window_.from} and {window_.to} {ZONE} · clear the selection on the chart to see all</span>}
      {!window_ && held.length === 0 && view === 'list' && <span><Kbd keys="e" /> expands the line under the cursor</span>}
      {view === 'patterns' && <span>one row is one message template · <Kbd keys="⏎" /> narrows the list to it</span>}
      {view === 'service' && <span>one row is one container · <Kbd keys="⏎" /> narrows the list to it</span>}
    </span>
  )

  const columns: LedgerColumn[] = view === 'list'
    ? ['time', 'level', 'project · service', 'message', ...trailing]
    : view === 'patterns'
      ? ['pattern', { label: 'lines', numeric: true }, 'share', 'first seen', 'last seen', '24h']
      : ['service', { label: 'lines', numeric: true }, { label: 'errors', numeric: true }, 'share']
  const grid = view === 'list'
    ? ['minmax(92px,max-content)', 'minmax(56px,max-content)', 'minmax(7rem,1fr)', 'minmax(14rem,5fr)', ...trailing.map((c) => TRAILING_GRID[c])].join(' ')
    : view === 'patterns'
      ? 'minmax(22rem,4fr) minmax(64px,max-content) minmax(6rem,1fr) minmax(88px,max-content) minmax(88px,max-content) minmax(6rem,1fr)'
      : 'minmax(14rem,2fr) minmax(64px,max-content) minmax(64px,max-content) minmax(8rem,1fr)'
  const rows = view === 'list' ? logRows : view === 'patterns' ? patternRows : serviceRows

  const actions = (
    <>
      <Segmented options={VIEWS} value={view} onChange={(v) => { patch({ lv: v === 'list' ? null : v }); setExpanded(null) }} className="h-7 [&>button]:h-7" />
      {view === 'list' && (
        <div className="relative">
          <button ref={colsBtn} type="button" onClick={() => setColsOpen((o) => !o)} aria-expanded={colsOpen} className="inline-flex h-7 items-center gap-1.5 border px-2 text-xs hover:bg-muted">
            <Columns3 aria-hidden className="h-3.5 w-3.5" /> columns <span className="font-mono text-muted-foreground">{trailing.length}</span>
          </button>
          <Drop anchor={colsBtn} open={colsOpen} width={280} label="trailing columns">
            <ul className="op-rows text-xs">
              {TRAILING.map((c) => (
                <li key={c}>
                  <button type="button" onClick={() => patch({ cols: (trailing.includes(c) ? trailing.filter((x) => x !== c) : [...trailing, c]).join(',') })}
                    className="flex w-full items-center gap-2 px-3 py-1.5 text-start hover:bg-muted">
                    <span aria-hidden className="w-3 text-center">{trailing.includes(c) ? '●' : '○'}</span>
                    <span className="font-mono">{c}</span>
                  </button>
                </li>
              ))}
            </ul>
            <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">the choice rides in the link, like the query · time, level, service and the message always show</p>
          </Drop>
        </div>
      )}
      <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'query copied', writeQuery(tokens, text) || 'no tokens: everything in the window')}><Copy /> copy query</Button>
      <div className="relative">
        <button ref={saveBtn} type="button" onClick={() => setSaveOpen((o) => !o)} aria-expanded={saveOpen} className="inline-flex h-7 items-center gap-1.5 border px-2 text-xs hover:bg-muted">save view</button>
        <Drop anchor={saveBtn} open={saveOpen} width={320} label="save this query">
          <form className="space-y-2 p-3 text-xs" onSubmit={(e) => {
            e.preventDefault()
            const name = saveName.trim()
            if (!name) return
            setSaved((prev) => [...prev.filter((s) => s.name !== name), { name, query: q }])
            setSaveName(''); setSaveOpen(false)
            notify('ok', `saved as ${name}`, 'it is in the saved-query picker beside the scope')
          }}>
            <label className="grid gap-1"><span className="op-label">name</span>
              <input value={saveName} onChange={(e) => setSaveName(e.target.value)} placeholder="billing health checks" className="h-8 border bg-background px-2 font-mono text-xs outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring" /></label>
            <p className="font-mono text-[10px] text-muted-foreground">{writeQuery(tokens, text) || 'no tokens: everything in the window'}</p>
            <div className="flex justify-end gap-2"><button type="button" onClick={() => setSaveOpen(false)} className="h-7 px-2 hover:bg-muted">cancel</button><button type="submit" className="op-primary h-7 border px-3">save view</button></div>
          </form>
        </Drop>
      </div>
      <div className="relative">
        <button ref={exportBtn} type="button" onClick={() => setExportOpen((o) => !o)} aria-expanded={exportOpen} className="inline-flex h-7 items-center gap-1.5 border px-2 text-xs hover:bg-muted">
          <Download aria-hidden className="h-3.5 w-3.5" /> export
        </button>
        <Drop anchor={exportBtn} open={exportOpen} width={340} label="export this query">
          <ul className="op-rows text-xs">
            {(['csv', 'json'] as const).map((f) => (
              <li key={f}>
                <button type="button" className="flex w-full items-center gap-2 px-3 py-1.5 text-start hover:bg-muted"
                  onClick={() => { setExportOpen(false); notify('ok', `export ${fmtNum(Math.min(list.length, 100000))} lines · ${f}`, writeQuery(tokens, text) || 'everything in the window') }}>
                  <span className="font-mono">export {fmtNum(Math.min(list.length, 100000))} lines · {f}</span>
                </button>
              </li>
            ))}
          </ul>
          <p className="border-t px-3 py-1.5 text-[11px] text-muted-foreground">the current query, capped at 100,000 lines · a bigger export is a scheduled one</p>
        </Drop>
      </div>
    </>
  )

  const ledger = (
    <Ledger
      status={null} dense={dense} columns={columns} grid={grid} rows={rows} total={view === 'list' ? list.length : rows.length}
      page={view === 'list' ? { page, pageSize: 100, total: list.length, onPage: setPage } : undefined}
      hint={hint} action={actions}
      state={fresh ? (
        <PageState state="unconfigured" title="No log source yet"
          missing="a running container, or a domain on the proxy. Deploy a project or attach a domain and every line its containers write arrives here within a second, keyed by service, node and deployment."
          example={<div className="space-y-1 font-mono text-[11px]"><p>× 21:00 billing-worker-2f1 health check GET /healthz timed out after 30s · dep_31c</p><p>◐ 20:41 api-gateway-7c4 slow query 5.2s: SELECT * FROM orders WHERE customer_id = $1</p><p>○ 20:40 proxy GET docs.acme.sh/guide 200 in 12ms</p></div>}
          settingsHref="/settings/store" settingsLabel="open the log store" />
      ) : rows.length === 0 ? (
        <PageState state="empty" title="No line matches this query"
          reason={`${fmtCount(inWindow.length, 'line')} in ${rangeLabel}, none matching ${writeQuery(tokens, text) || 'the window'}. Lines older than ${retentionLabel} were deleted, not hidden.`}
          next={<Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => { setQuery([], ''); setWindow(null) }}>clear the query</Button>} />
      ) : undefined}
      footer={view === 'list'
        ? <span>showing {fmtNum(rows.length)} of {fmtNum(list.length)} matching · {fmtNum(TOTAL_KEPT)} kept for {retentionLabel} · <Kbd keys="t" className="mx-1" /> opens the trace{range !== '90d' && <> · <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => { const i = RANGES.findIndex((r) => r.label === range); setRange(RANGES[Math.min(RANGES.length - 1, i + 1)].label); setWindow(null); setPage(1) }}>load older</button></>}</span>
        : <span>{fmtNum(rows.length)} {view === 'patterns' ? 'patterns' : 'containers'} over {fmtNum(list.length)} matching lines · <KbdPair keys={['j', 'k']} does={['down', 'up']} className="mx-1" /> · <Kbd keys="⏎" className="mx-1" /> narrow</span>} />
  )

  const expansion = view === 'list' && openLine && (
    <Section title="Expanded line" meta={`${openLine.id} · ${openLine.service}`}
      action={<button type="button" onClick={() => setExpanded(null)} className="text-xs text-muted-foreground hover:text-foreground">collapse <Kbd keys="e" /></button>}>
      <div className="space-y-3">
        <pre className="op-inset overflow-x-auto border px-3 py-2 font-mono text-[11px] leading-5">{pretty(openLine)}</pre>
        {openLine.fields && <KeyValue rows={Object.entries(openLine.fields).map(([k, v]) => ({ k, v: String(v) }))} />}
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => go(`log:${openLine.id}`)}>open the line</Button>
          {openLine.trace && <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => go(`trace:${openLine.trace}`)}>open the trace</Button>}
        </div>
      </div>
    </Section>
  )

  const body = <div className="space-y-6">{queryBar}{chart}{ledger}{expansion}</div>

  return (
    <div className="space-y-4">
      <PageTitle title="Logs" meta={fresh ? 'all projects · nothing recorded yet' : `${scope} · ${fmtNum(list.length)} lines`} />
      {status}
      {/* `items-start` is what lets the panel be sticky and still push: a stretched
          flex item is as tall as the list and has nothing left to stick to. */}
      <div className="flex min-w-0 items-start gap-6">
        {/* The aside is dropped by collapsing `Columns` to one track, never by
            swapping the wrapper: a different element here remounts the Ledger,
            and a remounted Ledger loses the cursor the panel is following. */}
        <div className="min-w-0 flex-1">
          {wide
            ? <Columns className={cn(!asideOpen && 'xl:grid-cols-[minmax(0,1fr)]')}><div>{body}</div>{asideOpen && <div>{facets}</div>}</Columns>
            : body}
        </div>
        {inspectLine && (
          <Inspector
            open label="log line inspector"
            state={LEVEL_STATE[inspectLine.level]} word={inspectLine.level}
            title={inspectLine.id}
            meta={`${fmtAbsolute(inspectLine.ts, { tz: ZONE, seconds: true })} · ${inspectLine.deploy}`}
            anchors={LOG_ANCHORS}
            onOpen={() => go(`log:${inspectLine.id}`)}
            onCopyLink={() => notify('ok', 'link copied', `?p=log:${inspectLine.id}`)}
            onClose={() => setInspect(null)}
            returnFocus={() => document.getElementById(`row-${inspectLine.id}`)}
          >
            <LogContent l={inspectLine} go={go} compact />
          </Inspector>
        )}
      </div>
    </div>
  )
}

/** The raw line as the container wrote it, or the JSON re-indented so a human can read it. */
function pretty(l: Log) {
  if (l.json) { try { return JSON.stringify(JSON.parse(l.msg), null, 2) } catch { return l.msg } }
  return l.raw ?? l.msg
}
/** The twenty lines either side, from the same container: what `log_chunks.line_offsets` exists for. */
function around(l: Log, all: Log[]) {
  const sameService = all.filter((x) => x.service === l.service).sort((a, b) => a.ts - b.ts)
  const at = sameService.findIndex((x) => x.id === l.id)
  return sameService.slice(Math.max(0, at - 20), at + 21)
}

// ── Correlation ──────────────────────────────────────────────────────
/* A line, its trace and its request are one event seen three ways, so all
   three are drawn where the reader already is rather than linked to. The
   link out stays — it is how you leave — but nobody should have to leave to
   find out whether the trace explains the line.

   The absence of a correlation is a fact, not a missing section: a line with
   no trace id still gets a Trace section that says so, what would be there,
   and where to turn it on. A section that vanishes teaches the reader that
   the feature does not exist. */

const toLine = (x: Log, current?: string): LogLine => ({
  t: new Date(x.ts).toISOString().slice(11, 19),
  level: x.level,
  source: x.id === current ? '▸ this line' : x.service,
  msg: x.msg,
})

/**
 * The lines carrying one trace id, for the trace record in Observe. Exported
 * from here because these are the fixtures the Logs screen itself reads: the
 * two screens have to agree about what a request said.
 */
export function traceLogLines(traceId: string): LogLine[] {
  return LINES.filter((l) => l.trace === traceId).sort((a, b) => a.ts - b.ts).map((l) => toLine(l))
}

/** A stable client address per request id: a fixture, not a random one, so a baseline is a fact about the design. */
const CLIENTS = ['203.0.113.9', '198.51.100.24', '203.0.113.77', '192.0.2.140'] as const
const clientOf = (req: string) => CLIENTS[[...req].reduce((a, c) => a + c.charCodeAt(0), 0) % CLIENTS.length]

/** The proxy access entry for the line's request id: method · route · status · latency · client · deploy. */
function accessOf(l: Log): KV[] | null {
  if (!l.request) return null
  const status = l.fields?.status === undefined ? 200 : Number(l.fields.status)
  return [
    { k: 'request', v: l.request, mono: true },
    // The method is only a fact when the line carried one: a defaulted "GET" beside a POST trace is a lie the reader cannot catch.
    l.fields?.method === undefined
      ? { k: 'route', v: String(l.fields?.route ?? '–'), mono: true }
      : { k: 'method · route', v: `${l.fields.method} ${l.fields.route ?? '/'}`, mono: true },
    { k: 'status', v: <Status state={status >= 500 ? 'error' : status >= 400 ? 'warn' : 'ok'} label={String(status)} /> },
    { k: 'latency', v: l.fields?.duration_ms === undefined ? '–' : `${fmtNum(Number(l.fields.duration_ms))}ms`, mono: true },
    { k: 'client', v: clientOf(l.request), mono: true },
    { k: 'deployment', v: l.deploy, mono: true },
  ]
}

const flatten = (spans: VizSpan[]): VizSpan[] => spans.flatMap((s) => [s, ...flatten(s.children ?? [])])
/**
 * Which span wrote this line: the one the line names (`address.normalize` for a
 * message about `AddressForm.tsx`), then the one serving its route, then the
 * root. Never a guess dressed as a fact — a line that names nothing belongs to
 * the request span, which is exactly what the root is.
 */
function emitting(l: Log, spans: VizSpan[]): string {
  const flat = flatten(spans)
  const msg = l.msg.toLowerCase()
  const named = flat.find((s) => { const w = s.name.toLowerCase().split(/[.\s]/)[0]; return w.length >= 4 && msg.includes(w) && s !== flat[0] })
  const byRoute = l.fields?.route === undefined ? undefined : flat.find((s) => s.name.endsWith(String(l.fields!.route)))
  return (named ?? byRoute ?? flat.find((s) => l.msg.includes(s.name)) ?? flat[0]).id
}
/** `▸ this line` on the span that emitted it — the same mark `LogLines` puts on the current line. */
function markSpan(spans: VizSpan[], id: string): VizSpan[] {
  return spans.map((s) => ({ ...s, service: s.id === id ? '▸ this line' : s.service, children: s.children && markSpan(s.children, id) }))
}

/** The Inspector's toc, and the ids the record's own sections carry, in one list. */
const LOG_ANCHORS: InspectorAnchor[] = [
  { id: 'log-fields', label: 'fields' },
  { id: 'log-trace', label: 'trace' },
  { id: 'log-request', label: 'request' },
  { id: 'log-context', label: 'context' },
]

/**
 * One log line, read. The record page wraps this in the record template
 * (title, meta, verdict, Lede, aside) and the Inspector wraps it in the
 * panel; there is one copy of it, because two would drift and the panel
 * would quietly become the poorer of the two.
 */
function LogContent({ l, go, compact = false }: {
  l: Log
  go: (v: string) => void
  /** The panel is 520px wide with no Lede above it: it draws the facts the record's Lede carries, keys over values, and a shorter context pane. */
  compact?: boolean
}) {
  const trace = checkoutTrace()
  const emit = l.trace ? emitting(l, trace.spans) : null
  const access = accessOf(l)
  const context = around(l, LINES).map((x) => toLine(x, l.id))
  return (
    <>
      <div id="log-fields" className="op-block">
        {compact && (
          <Section title="Facts" meta="what wrote it">
            <KeyValue compact rows={[
              { k: 'host · node', v: l.node, mono: true },
              { k: 'service', v: l.service, mono: true },
              { k: 'source', v: l.source, mono: true },
              { k: 'deployment', v: l.deploy, mono: true },
            ]} />
          </Section>
        )}
        <Section title="The line" meta={l.json ? 'json, re-indented' : l.raw ? 'as the container wrote it' : 'plain text'}>
          <pre className="op-inset overflow-x-auto border px-3 py-2 font-mono text-[11px] leading-5">{pretty(l)}</pre>
        </Section>
        {l.fields && (
          <Section title="Structured fields" meta={fmtCount(Object.keys(l.fields).length, 'field')}>
            <KeyValue compact={compact} rows={Object.entries(l.fields).map(([k, v]) => ({ k, v: String(v) }))} />
          </Section>
        )}
      </div>
      <div id="log-trace" className="op-block">
        <Section
          title="Trace" meta={l.trace ? `${l.trace.slice(0, 8)} · ${trace.span_count} spans · ${trace.total_ms}ms` : 'nothing to correlate'}
          action={l.trace ? <button type="button" onClick={() => go(`trace:${l.trace}`)} className="text-xs text-muted-foreground hover:text-foreground">open the trace</button> : undefined}
        >
          {l.trace && emit
            ? <Waterfall spans={markSpan(trace.spans, emit)} total_ms={trace.total_ms} selected={emit} />
            : <p className="text-xs text-muted-foreground">
              no trace id on this line · the SDK adds one when tracing is on. With it this section is the whole request — <span className="font-mono">{trace.root} {trace.total_ms}ms across {trace.span_count} spans</span> — with the span that wrote this line marked <span className="font-mono">▸ this line</span>. <Phrase onClick={() => go('settings:store')}>Turn tracing on in Settings › Store</Phrase>.
            </p>}
        </Section>
      </div>
      <div id="log-request" className="op-block">
        <Section
          title="Request" meta={l.request ? 'the proxy access entry' : 'nothing to correlate'}
          action={l.request ? <button type="button" onClick={() => go('proxy')} className="text-xs text-muted-foreground hover:text-foreground">open in proxy</button> : undefined}
        >
          {access
            ? <KeyValue compact={compact} rows={access} />
            : <p className="text-xs text-muted-foreground">
              no request id on this line · the proxy stamps one on everything it serves, and the SDK passes it through. With it this section is the access entry — <span className="font-mono">GET /api/products 200 · 42ms · 203.0.113.9 · dep_91a</span> — so a line and the request that caused it read together. <Phrase onClick={() => go('settings:routes')}>Put this service behind the proxy in Settings › Custom routes</Phrase>.
            </p>}
        </Section>
      </div>
      <div id="log-context" className="op-block">
        <Section title="Around it" meta="±20 lines from the same container · ▸ marks this one">
          <LogLines lines={context} height={compact ? 220 : 320} />
        </Section>
      </div>
    </>
  )
}

// ── The record ───────────────────────────────────────────────────────

export function LogRecord({ id, go, notify }: { id: string; go: (v: string) => void; notify: Notify }) {
  const l = LINES.find((x) => x.id === id) ?? LINES[0]
  const similar = LINES.filter((x) => x.msg === l.msg && x.ts >= l.ts - HOUR && x.ts <= l.ts).length
  const facts: KV[] = [
    { k: 'level', v: <Status state={LEVEL_STATE[l.level]} label={l.level} /> },
    { k: 'service', v: l.service, mono: true },
    { k: 'deployment', v: l.deploy, mono: true },
    { k: 'node', v: l.node, mono: true },
    { k: 'trace', v: l.trace ? l.trace.slice(0, 8) : '–', mono: true },
    { k: 'written', v: fmtRelative(l.ts, NOW), mono: true },
  ]
  const verdict = l.level === 'error'
    ? <StatusLine state="error" more={{ label: `${fmtNum(similar)} like it in the hour before`, onClick: () => go(`logs:service:${l.service}`) }}>
      {l.service} has said this since dep_31c shipped. <Phrase onClick={() => go(`logs:deploy:${l.deploy}`)}>Read the lines around the deploy</Phrase>, then roll {l.project} back or fix the check.
    </StatusLine>
    : l.level === 'warn'
      ? <StatusLine state="warn">Slow, not failing: nothing was returned to a caller as an error. Worth an alert if it keeps up.</StatusLine>
      : <StatusLine state="ok">Nothing to do: an ordinary {l.source} line, kept so the twenty around it can be read.</StatusLine>
  return (
    <Detail
      title={<span className="min-w-0 font-mono">{l.msg.length > 96 ? `${l.msg.slice(0, 96)}…` : l.msg}</span>}
      mark={<ProjectMark name={l.project} icon={PROJECT_ICONS[l.project]} size={24} />}
      meta={`${l.id} · ${l.project} · ${l.env}`}
      status={verdict}
      lede={<Lede state={LEVEL_STATE[l.level]} word={l.level} facts={facts}>{l.source} line, {fmtAbsolute(l.ts, { tz: ZONE, seconds: true })}</Lede>}
      actions={<>
        <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'line copied', l.id)}><Copy /> copy line</Button>
        {l.trace && <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => go(`trace:${l.trace}`)}><Waypoints /> open in trace</Button>}
        <Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'alert created', `level:error service:${l.service} · notifies #ops · remove it on Settings › Alerts`)}><Bell /> create alert from this query</Button>
      </>}
    >
      <Columns>
        <div>
          <LogContent l={l} go={go} />
        </div>
        <div>
          <Section title="Go from here">
            <ul className="space-y-1.5 text-xs">
              {l.trace && <li><Phrase onClick={() => go(`trace:${l.trace}`)}>the trace it belongs to</Phrase></li>}
              <li><Phrase onClick={() => go(`deploy:${l.deploy}`)}>the deployment that was live</Phrase></li>
              {l.level === 'error' && <li><Phrase onClick={() => go('issue:i_4821')}>the issue error tracking grouped it under</Phrase></li>}
              <li><Phrase onClick={() => go(`logs:service:${l.service}`)}>every line this container wrote</Phrase></li>
              {l.request && <li><Phrase onClick={() => go(`logs:request:${l.request}`)}>the rest of {l.request}</Phrase></li>}
            </ul>
          </Section>
          <Section title="Similar lines">
            <p className="text-xs text-muted-foreground">
              <Phrase onClick={() => go(`logs:pattern:${template(l.msg)}`)}>{fmtCount(similar, 'line')} in the hour before this one</Phrase> say exactly this. One alert covers all of them.
            </p>
          </Section>
        </div>
      </Columns>
    </Detail>
  )
}

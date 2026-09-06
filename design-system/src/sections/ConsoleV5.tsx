// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { Link, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import {
  Activity,
  ArrowLeftRight,
  Cog,
  BarChart3,
  Bell,
  Database,
  FolderOpen,
  Gauge,
  GitBranch,
  Globe,
  HardDrive,
  Mail,
  Menu,
  Plus,
  Rocket,
  RotateCcw,
  Rows3,
  ScrollText,
  Maximize2,
  Minimize2,
  Search,
  ShieldCheck,
  Terminal,
  Trash2,
  Waypoints,
  X,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from '@/components/ui/command'
import { Input } from '@/components/ui/input'
import { LogViewer, type LogLine } from '@/components/ui/log-viewer'
import { Skeleton } from '@/components/ui/skeleton'
import { Sparkline } from '@/components/ui/sparkline'
import { LogoMark } from '@/components/Logo'
import {
  ChartFooter, Detail, EchoDialog, Field, Kbd, Ledger, Lede, Metric, MetricGrid, MOD, Num, PageState, PageTitle, Section, ShellSlotsProvider, Drop, AttentionHost, ProjectMark, Phrase, Picker, type PickerOption, RangePicker,
  Segmented, Settings, STATE_RANK, Status, StatusLine, TimeChart, type LedgerRow, type Range, type State,
} from '@/components/op'
import { DeploysTab, EnvironmentsTab, VariablesTab } from '@/sections/ConsoleV5Env'
import { DeploymentScreen } from '@/sections/ConsoleV5Deploy'
import { NodeScreen } from '@/sections/ConsoleV5Nodes'
import { MetricsScreen, SandboxScreen, SandboxesScreen, TraceScreen, TracesScreen } from '@/sections/ConsoleV5Observe'
import { EmailDetailScreen, EmailDomainScreen, EmailScreen } from './ConsoleV5Email'
import { DatabaseScreen } from './ConsoleV5Database'
import { ErrorsScreen, IssueScreen } from './ConsoleV5Errors'
import { ProxyScreen } from './ConsoleV5Proxy'
import { SettingsHub, SettingsPage } from './ConsoleV5Settings'
import { useFresh } from './console-fresh'
import { PROJECT_ICONS as ICONS } from './console-projects'
import { AnalyticsScreen, EventScreen, MonitorScreen, UptimeScreen } from './ConsoleV5Analytics'
import { BackupsScreen, GitProvidersScreen, GitProviderScreen, SecurityScreen, ScanScreen } from './ConsoleV5Admin'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   /v5 — the twelve answers, as code.

   What is new versus v4 is structural, not visual:
   · Three page templates (Ledger, Detail, Settings). Every screen is one
     of them. No screen starts from a blank div.
   · One PageState component with four states: loading, empty,
     unconfigured, error. Every failure names the reason and the next step.
   · A fifth status, "sampled". Pricing promises the console says when
     telemetry is head-sampled past the plan allowance; it never drops it
     silently. So the state exists in the glyph set, the status line and
     on the chart.
   · Retention horizon on every time axis. The horizon differs per plan
     (30d / 90d / 13mo / as configured), so the axis says what it is.
   · Metric tiles that must name their baseline. No bare deltas.
   · Decisions frozen: no accent axis, radius 0.25rem, ink borders, density
     comfortable by default and remembered.
   · A plan switcher in the header so you can see how the same screens
     behave on self-hosted, Starter, Team and Business.
   ──────────────────────────────────────────────────────────────────────── */

const SKIN = 'operator ink v5'

function seeded(seed: number) {
  let s = seed
  return () => {
    s = (s * 1664525 + 1013904223) % 4294967296
    return s / 4294967296
  }
}
const rand = seeded(23)

// ── Status vocabulary: five states, always glyph + word ─────────────────

// ── Plans: pricing.md as design input ──────────────────────────────────

type PlanId = 'selfhost' | 'starter' | 'team' | 'business'
const PLANS: Record<PlanId, { label: string; retention: string; retentionDays: number; ingest: string | null; ingestGb: number | null; pitr: string }> = {
  selfhost: { label: 'self-hosted', retention: 'as configured · 90d', retentionDays: 90, ingest: null, ingestGb: null, pitr: 'as configured' },
  starter: { label: 'Cloud Starter', retention: '30d', retentionDays: 30, ingest: '10 GB/mo', ingestGb: 10, pitr: '7d' },
  team: { label: 'Cloud Team', retention: '90d', retentionDays: 90, ingest: '100 GB/mo', ingestGb: 100, pitr: '30d' },
  business: { label: 'Cloud Business', retention: '13 months', retentionDays: 395, ingest: '1 TB/mo', ingestGb: 1000, pitr: '90d' },
}
const INGEST_USED_GB = 11.4 // this month, all projects
const PlanContext = createContext<{ plan: PlanId; setPlan: (p: PlanId) => void }>({ plan: 'selfhost', setPlan: () => {} })
function usePlan() {
  const { plan } = useContext(PlanContext)
  const p = PLANS[plan]
  const sampled = p.ingestGb !== null && INGEST_USED_GB > p.ingestGb
  return { id: plan, ...p, sampled }
}

// ── Data ────────────────────────────────────────────────────────────────

function diurnal(i: number, scale: number, noise: number) {
  const h = i / 2
  const day = Math.max(0, Math.sin(((h - 7) / 24) * Math.PI * 2)) ** 1.2
  return Math.round(scale * (0.25 + day) + (rand() - 0.5) * noise)
}


/** Demo favicons as data URIs. In the console these come from /api/projects/{id}/icon, fetched and stored server-side after a deploy. */
/* Every project carries its own current deploy, branch, repo and shape: the
   record page derives its meta, its lede, its recent deploys and its incident
   thread from the project, never from one shared constant. A project that
   shows another project's deploy tag is the fastest way to make a console
   look like a mock. */
const PROJECTS = [
  { name: 'api-gateway', kind: 'app', repo: 'api-gateway', domain: 'api.acme.sh', replicas: '3 × 512 MB', branch: 'main', env: 'production', state: 'warn' as State, deployed: '41m ago', dep: 'dep_91a', commit: '9bc61c0', msg: 'feat(checkout): new address form', by: 'maya', dur: '48s', prev: { dep: 'dep_90e', at: '10h ago', commit: '4f21a8d', msg: 'perf(router): cache edge lookups', by: 'maya', dur: '41s' }, err: 0.61, visitors: 30800, cert: '6d', note: 'error rate above 0.5% since dep_91a' },
  { name: 'billing-worker', kind: 'worker', repo: 'billing-worker', domain: '—', replicas: '2 × 1 GB', branch: 'main', env: 'production', state: 'error' as State, deployed: '2h ago', dep: 'dep_31c', commit: '5d90b12', msg: 'fix(invoices): retry stripe webhooks', by: 'jules', dur: '39s', prev: { dep: 'dep_30f', at: '2d ago', commit: 'a77e410', msg: 'chore(deps): bump stripe to 18.2', by: 'maya', dur: '37s' }, err: 3.4, visitors: 0, cert: '—', note: 'health check failing since dep_31c' },
  { name: 'acme-storefront', kind: 'app', repo: 'acme-storefront', domain: 'acme.sh', replicas: '4 × 512 MB', branch: 'main', env: 'production', state: 'ok' as State, deployed: '3d ago', dep: 'dep_88c', commit: 'c0ffee1', msg: 'chore: bump deps', by: 'jules', dur: '52s', prev: { dep: 'dep_88a', at: '5d ago', commit: '1f7d902', msg: 'feat(cart): saved carts', by: 'jules', dur: '58s' }, err: 0.04, visitors: 12400, cert: '71d', note: '' },
  { name: 'acme-crm', kind: 'app', repo: 'acme-crm', domain: 'crm.acme.sh', replicas: '2 × 512 MB', branch: 'main', env: 'production', state: 'ok' as State, deployed: '4d ago', dep: 'dep_87f', commit: 'a1b2c3d', msg: 'fix(dialog): null focus target', by: 'jules', dur: '44s', prev: { dep: 'dep_87c', at: '9d ago', commit: '77e0a13', msg: 'feat(leads): bulk import', by: 'maya', dur: '46s' }, err: 0.11, visitors: 4090, cert: '58d', note: '' },
  { name: 'docs', kind: 'static', repo: 'docs', domain: 'docs.acme.sh', replicas: '1 × 256 MB', branch: 'main', env: 'production', state: 'ok' as State, deployed: '15d ago', dep: 'dep_80a', commit: '3c9de11', msg: 'docs: rewrite the deploy guide', by: 'jules', dur: '22s', prev: { dep: 'dep_79f', at: '22d ago', commit: 'b41c780', msg: 'docs: fix broken links', by: 'jules', dur: '20s' }, err: 0.0, visitors: 2210, cert: '44d', note: '' },
  { name: 'acme-web', kind: 'app', repo: 'acme-web', domain: '—', replicas: '—', branch: 'main', env: 'staging', state: 'idle' as State, deployed: 'never', dep: '—', commit: '—', msg: '', by: '—', dur: '—', prev: null, err: 0, visitors: 0, cert: '—', note: 'not deployed' },
].map((p) => ({ ...p, icon: ICONS[p.name], spark: Array.from({ length: 24 }, (_, i) => Math.max(1, diurnal(i * 2, p.visitors / 40 + 4, 6))) }))
type Project = (typeof PROJECTS)[number]

/** A branch is a kind of thing, so it carries a kind icon, never a colour (brand §6, "icons say what"). */
function Branch({ name }: { name: string }) {
  return <span className="inline-flex min-w-0 items-baseline gap-1"><GitBranch aria-hidden className="h-3 w-3 shrink-0 translate-y-0.5 text-muted-foreground" /><span className="min-w-0 truncate">{name}</span></span>
}

/** Case-insensitive substring match over the fields a filter box searches. */
export const matches = (q: string, ...fields: (string | undefined | null)[]) => { const n = q.trim().toLowerCase(); return !n || fields.some((f) => (f ?? '').toLowerCase().includes(n)) }


/** "4.2 GB" → bytes-ish number for sorting. */
export const sizeNum = (s: string) => { const [n, u] = s.split(' '); return Number(n) * ({ MB: 1, GB: 1024, TB: 1024 * 1024 }[u] ?? 1) }
/** "41m ago" / "2h ago" / "3d ago" → minutes for sorting. */
export const agoNum = (s: string) => { const m = /(\d+)(m|h|d|w)/.exec(s); if (!m) return null; return Number(m[1]) * ({ m: 1, h: 60, d: 1440, w: 10080 }[m[2]] ?? 1) }

const DATABASES = [
  { name: 'acme-pg', engine: 'PostgreSQL 18', state: 'ok' as State, size: '4.2 GB', backup: '2h ago', pitr: true },
  { name: 'sessions-redis', engine: 'Redis 7', state: 'ok' as State, size: '180 MB', backup: '2h ago', pitr: false },
  { name: 'events-ch', engine: 'ClickHouse 24', state: 'warn' as State, size: '38 GB', backup: '3d ago', pitr: false, note: 'backup older than 24h' },
]

const SERIES = Array.from({ length: 48 }, (_, i) => ({
  t: `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`,
  req: diurnal(i, 1100, 70) + (i >= 20 ? Math.min(1, (i - 19) / 3) * 300 : 0),
  prev: diurnal(i, 1000, 60),
}))

/** The project's own deploy history: the current one and the one before it, on the chart's markers. */
type Deploy = { id: string; x: string; at: string; commit: string; branch: string; by: string; state: State; msg: string; dur: string }
function deploysOf(p: Project): Deploy[] {
  if (p.dep === '—') return []
  return [
    { id: p.dep, x: '20:30', at: p.deployed, commit: p.commit, branch: p.branch, by: p.by, state: p.state === 'idle' ? 'ok' : p.state, msg: p.msg, dur: p.dur },
    ...(p.prev ? [{ id: p.prev.dep, x: '10:00', at: p.prev.at, commit: p.prev.commit, branch: p.branch, by: p.prev.by, state: 'ok' as State, msg: p.prev.msg, dur: p.prev.dur }] : []),
  ]
}

/** Where an incident row goes when it is opened. `null` is a row with nothing behind it yet, and it says so instead of drawing a link. */
type ThreadLink = 'deploy' | 'error' | 'replay' | 'alerts' | null
type ThreadItem = { at: string; kind: string; title: string; detail: string; link: ThreadLink }
/** Deploy, error, replay and alert as one thread — the project's own, not a shared constant. */
function threadOf(p: Project): ThreadItem[] {
  const deployed: ThreadItem = { at: '20:34:12', kind: 'deploy', title: `${p.dep} deployed to ${p.env}`, detail: `${p.branch}@${p.commit} · ${p.msg} · ${p.dur} build`, link: 'deploy' }
  if (p.state === 'ok') return [
    deployed,
    { at: '20:34:41', kind: 'health', title: 'health check passed', detail: `/healthz · 200 in 41ms · ${p.replicas}`, link: null },
    { at: '20:35:02', kind: 'cert', title: `certificate for ${p.domain} renewed`, detail: `valid ${p.cert} more · renews automatically 30 days before expiry`, link: null },
  ]
  const failure: ThreadItem = p.state === 'error'
    ? { at: '20:38:41', kind: 'error', title: 'health check /healthz timed out after 30s', detail: `${p.replicas} · 3 consecutive failures · container restarted twice`, link: 'error' }
    : { at: '20:38:41', kind: 'error', title: "TypeError: cannot read properties of undefined (reading 'id')", detail: 'src/checkout/AddressForm.tsx:88 · first seen · 31 events · 12 users', link: 'error' }
  return [
    deployed,
    failure,
    ...(p.visitors > 0 ? [{ at: '20:39:05', kind: 'replay', title: 'session sess_9f31c hit it at 00:41', detail: '/checkout → submit → error · Safari 17 · 1:24 long', link: 'replay' as ThreadLink }] : []),
    { at: '20:40:00', kind: 'alert', title: `error rate crossed ${p.state === 'error' ? '1%' : '0.5%'}`, detail: `now ${p.err}% · threshold set on 2026-06-02 · notified #ops`, link: 'alerts' },
  ]
}

const LOG_TEMPLATE: LogLine[] = [
  { ts: '20:33:24', level: 'info', msg: 'build started', fields: { commit: '9bc61c0', branch: 'main' } },
  { ts: '20:33:25', level: 'info', msg: 'pulling base image', fields: { image: 'rust:1.91-slim' } },
  { ts: '20:33:31', level: 'debug', msg: 'layer cached', fields: { id: 'sha256:4f2a…' } },
  { ts: '20:33:32', level: 'info', msg: 'cargo build --release' },
  { ts: '20:34:05', level: 'warn', msg: 'unused import: `std::fmt`', fields: { file: 'src/router.rs', line: 12 } },
  { ts: '20:34:10', level: 'info', msg: 'build finished', fields: { took: '48.1s' } },
  { ts: '20:34:11', level: 'info', msg: 'health check passed', fields: { path: '/healthz', status: 200 } },
  { ts: '20:34:12', level: 'info', msg: 'routing traffic', fields: { from: 'dep_90e', to: 'dep_91a' } },
]

// ── Notifications ──────────────────────────────────────────────────────

/* A change that is cheap to reverse is confirmed by its consequence, not by a dialog: it happens,
   and the toast carries the undo. `undo` is optional, so a notify without one is unchanged. */
type Note = { id: number; level: 'ok' | 'warn' | 'err'; msg: string; detail?: string; ts: string; undo?: () => void }
const NotesContext = createContext<{ notes: Note[]; push: (n: Omit<Note, 'id' | 'ts'>) => void }>({ notes: [], push: () => {} })
function useNotify() {
  const { push } = useContext(NotesContext)
  return useCallback((level: Note['level'], msg: string, detail?: string, undo?: () => void) => push({ level, msg, detail, undo }), [push])
}
function NoteRow({ n, onUndo }: { n: Note; onUndo?: () => void }) {
  return (
    <div className="flex items-start gap-2 font-mono text-xs">
      <span className={cn('w-8 shrink-0', n.level === 'ok' && 'text-success', n.level === 'warn' && 'text-warning', n.level === 'err' && 'text-destructive')}>{n.level}</span>
      <span className="min-w-0 flex-1">
        <span className="block">{n.msg}</span>
        {n.detail && <span className="block truncate text-muted-foreground">{n.detail}</span>}
      </span>
      {n.undo && <button type="button" className="shrink-0 underline underline-offset-4 hover:text-foreground" onClick={() => { n.undo?.(); onUndo?.() }}>undo</button>}
      <span className="shrink-0 tabular-nums text-muted-foreground">{n.ts}</span>
    </div>
  )
}

// ── Primitives the templates are built from ─────────────────────────────

// ── Template 1: Ledger ─────────────────────────────────────────────────

// ── Template 2: Detail ─────────────────────────────────────────────────

// ── Template 3: Settings ───────────────────────────────────────────────

// ── Screens ────────────────────────────────────────────────────────────

function ProjectsScreen({ go, dense }: { go: (v: string) => void; dense: boolean }) {
  const [q, setQ] = useState('')
  const plan = usePlan()
  const list = useMemo(() => PROJECTS.filter((p) => matches(q, p.name, p.note)).sort((a, b) => STATE_RANK[a.state] - STATE_RANK[b.state]), [q])
  const attention = PROJECTS.filter((p) => p.state === 'warn' || p.state === 'error')
  const rows: LedgerRow[] = list.map((p) => ({
    id: p.name, state: p.state, onOpen: () => go(p.name),
    sort: { name: p.name, deployed: agoNum(p.deployed), visitors: p.visitors || null, err: p.dep === '—' ? null : p.err, cert: p.cert === '—' ? null : Number(p.cert) },
    mobile: <><span className="flex items-center gap-2 truncate font-medium"><ProjectMark name={p.name} icon={p.icon} />{p.name}</span><span className="block truncate text-[11px] text-muted-foreground">{p.note || `${p.env} · deployed ${p.deployed}`}</span></>,
    cells: [
      <span className="flex min-w-0 items-center gap-2 font-medium"><ProjectMark name={p.name} icon={p.icon} /><span className="truncate">{p.name}</span></span>,
      <Status state={p.state} label={p.note || p.env} />,
      <span className="text-muted-foreground">{p.deployed}{p.dep !== '—' && <span className="font-mono"> · {p.dep}</span>}</span>,
      <span className="flex items-center justify-between gap-2"><Num value={p.visitors || null} />{p.visitors > 0 && <Sparkline values={p.spark} height={dense ? 10 : 14} />}</span>,
      <Num value={p.dep === '—' ? null : p.err.toFixed(2)} unit="%" />,
      <Num value={p.cert === '—' ? null : p.cert} />,
    ],
  }))
  return (
    <Ledger
      title="Projects" meta={`${PROJECTS.length} projects · ${plan.label}`}
      status={
        <StatusLine state={attention.length ? 'error' : plan.sampled ? 'sampled' : 'ok'} more={attention.length > 1 ? { label: `+${attention.length - 1 + (plan.sampled ? 1 : 0)} more`, items: [
            { state: 'warn', children: <><Phrase onClick={() => go('api-gateway')}>api-gateway</Phrase> error rate 0.61% since dep_91a.</> },
            ...(plan.sampled ? [{ state: 'sampled' as State, children: <>Telemetry is sampled 1 in 4 since 14:00, {plan.ingest} allowance reached.</> }] : []),
          ] } : undefined}>
          {attention.length ? <><Phrase onClick={() => go('billing-worker')}>billing-worker</Phrase> is failing health checks.</> : plan.sampled ? <>Telemetry is sampled 1 in 4 since 14:00.</> : <>All {PROJECTS.length} projects healthy.</>}
        </StatusLine>
      }
      columns={[{ label: 'project', key: 'name' }, 'status', { label: 'last deploy', key: 'deployed' }, { label: 'visitors · 24h', key: 'visitors' }, { label: 'error rate', key: 'err', numeric: true }, { label: 'cert', key: 'cert', numeric: true }]} grid="1.4fr 1fr 1fr 140px 100px 80px"
      rows={rows} total={PROJECTS.length} filter={q} onFilter={setQ} placeholder="filter projects" hint="needs attention first, then last deploy" dense={dense}
      action={<Button size="sm" className="op-primary h-8 text-xs"><Plus /> <span className="hidden sm:inline">new project</span></Button>}
    />
  )
}

function DatabasesScreen({ dense, go }: { dense: boolean; go: (v: string) => void }) {
  const [q, setQ] = useState('')
  const plan = usePlan()
  const list = DATABASES.filter((d) => matches(q, d.name, d.engine))
  const rows: LedgerRow[] = list.map((d) => ({
    id: d.name, state: d.state, onOpen: () => go(`db:${d.name}`),
    sort: { name: d.name, size: sizeNum(d.size), backup: agoNum(d.backup) },
    mobile: <><span className="block truncate font-medium">{d.name}</span><span className="block truncate text-[11px] text-muted-foreground">{d.note || d.engine}</span></>,
    cells: [<span className="font-medium">{d.name}</span>, <Status state={d.state} label={d.note || d.engine} />, <Num value={d.size} />, <span className="text-muted-foreground">{d.backup}</span>, <span className="font-mono">{d.pitr ? plan.pitr : '–'}</span>],
  }))
  return (
    <Ledger
      title="Databases" meta={`3 managed · point-in-time recovery ${plan.pitr}`}
      status={<StatusLine state="warn"><Phrase>events-ch</Phrase> backup is 3 days old.</StatusLine>}
      columns={[{ label: 'database', key: 'name' }, 'status', { label: 'size', key: 'size', numeric: true }, { label: 'last backup', key: 'backup' }, 'pitr']} grid="1.4fr 1.4fr 100px 120px 100px"
      rows={rows} total={DATABASES.length} filter={q} onFilter={setQ} placeholder="filter databases" dense={dense}
      action={<Button size="sm" className="op-primary h-8 text-xs"><Plus /> <span className="hidden sm:inline">new database</span></Button>}
    />
  )
}

// ── Project detail ─────────────────────────────────────────────────────

const RANGES: readonly Range[] = [{ label: '24h', days: 1 }, { label: '7d', days: 7 }, { label: '30d', days: 30 }, { label: '90d', days: 90 }, { label: '13mo', days: 395 }]

function RequestsChart({ hot, onHot, compare, deploys }: { hot: string | null; onHot: (id: string | null) => void; compare: boolean; deploys: Deploy[] }) {
  const plan = usePlan()
  const notify = useNotify()
  const [range, setRange] = useState('24h')
  return (
    <Section title="Requests" meta="30 min buckets"
      action={<RangePicker ranges={RANGES} value={range} onChange={setRange} retentionDays={plan.retentionDays} retentionLabel={plan.retention}
          onGated={(r) => notify('warn', `${r.label} is beyond this plan's retention`, plan.id === 'selfhost' ? `raise retention in settings · currently ${plan.retention}` : `${plan.label} keeps ${plan.retention} · Team keeps 90d, Business 13 months`)} />}>
      <div className="space-y-2">
      <TimeChart
        data={SERIES} unit="req" yTicks={[0, 1000, 2000]} xInterval={11}
        series={compare ? [{ key: 'req', name: 'requests' }, { key: 'prev', name: 'yesterday' }] : [{ key: 'req', name: 'requests' }]}
        markers={deploys.filter((d) => d.x).map((d) => ({ id: d.id, x: d.x }))} hot={hot} onHot={onHot}
        sampled={plan.sampled ? { from: '14:00', to: '23:30', label: 'sampled 1 in 4' } : undefined}
        readoutFormat={(p) => `${p.t} · ${Number(p.req).toLocaleString()} req${compare ? ` · yesterday ${Number(p.prev).toLocaleString()}` : ''}`}
      />
      <ChartFooter>
        <span>showing {range}</span>
        <span>· retention {plan.retention}</span>
        <span>· ┆ deploy</span>
        {plan.sampled && <span>· ◌ sampled since 14:00, {plan.ingest} allowance reached, {INGEST_USED_GB} GB used</span>}
      </ChartFooter>
      </div>
    </Section>
  )
}

const TABS = ['overview', 'deploys', 'environments', 'variables', 'logs', 'settings'] as const
type Tab = (typeof TABS)[number]

/** Branches as the git provider returns them: default first, then by last commit. */
export const BRANCHES: PickerOption[] = [
  { value: 'main', group: 'default', meta: 'e4d1f0a · 41m ago', keywords: 'master trunk' },
  { value: 'staging', group: 'recent', meta: '9bc61c0 · 2h ago' },
  { value: 'feat/checkout-address', group: 'recent', meta: 'b7c9d21 · 6h ago', keywords: '212' },
  { value: 'fix/retry-stripe-webhooks', group: 'recent', meta: '7a11c3e · yesterday' },
  { value: 'feat/edge-cache', group: 'recent', meta: 'c0ffee1 · 2d ago' },
  { value: 'release/1.4', group: 'all', meta: '3 weeks ago' },
  { value: 'release/1.3', group: 'all', meta: '2 months ago' },
  { value: 'chore/deps-2026-08', group: 'all', meta: '2 months ago' },
  { value: 'spike/otel-metrics', group: 'all', meta: '4 months ago' },
]

function ProjectScreen({ name, dense, go }: { name: string; dense: boolean; go: (v: string) => void }) {
  const notify = useNotify()
  const plan = usePlan()
  const project = PROJECTS.find((p) => p.name === name)
  const [tab, setTab] = useState<Tab>('overview')
  const [hot, setHot] = useState<string | null>(null)
  const [compare, setCompare] = useState<'none' | 'yesterday'>('none')
  const [rolledBack, setRolledBack] = useState(false)
  const [lines, setLines] = useState<LogLine[]>([])
  const [loading, setLoading] = useState(true)
  const deployBtn = useRef<HTMLButtonElement>(null)
  // settings form
  const [form, setForm] = useState({ branch: 'main', domain: 'api.acme.sh', health: '/healthz' })
  const [saved, setSaved] = useState(form)
  const dirty = JSON.stringify(form) !== JSON.stringify(saved)

  useEffect(() => { setLoading(true); const t = window.setTimeout(() => setLoading(false), 350); return () => window.clearTimeout(t) }, [name])
  useEffect(() => { let i = 0; const id = window.setInterval(() => setLines((prev) => (i < LOG_TEMPLATE.length ? [...prev, LOG_TEMPLATE[i++]] : prev)), 500); return () => window.clearInterval(id) }, [])
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); deployBtn.current?.click() } }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  if (!project) return null

  if (project.state === 'idle') {
    return (
      <div className="space-y-6">
        <PageTitle title={project.name} mark={<ProjectMark name={project.name} icon={project.icon} size={24} />} meta="production · never deployed" />
        <StatusLine state="idle">{project.name} has never deployed.</StatusLine>
        <PageState
          state="unconfigured"
          title="Nothing deployed yet"
          missing="a branch to auto-deploy and a production domain. Push to main or deploy now."
          settingsHref="/settings" settingsLabel="pick a branch"
          example={
            <div className="space-y-2 font-mono text-[11px]">
              <p>● {project.name} · production · deployed 2m ago dep_001 · error rate 0.00%</p>
              <div className="flex h-16 items-end gap-px">{Array.from({ length: 40 }, (_, i) => <span key={i} className="flex-1 bg-foreground/60" style={{ height: `${20 + Math.abs(Math.sin(i / 5)) * 70}%` }} />)}</div>
              <p className="text-muted-foreground">requests / 30 min · deploy markers on the axis · retention {plan.retention}</p>
            </div>
          }
        />
        <Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'deploy started', `${project.name} · main@e4d1f0a`)}><Rocket /> deploy now <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button>
      </div>
    )
  }

  const deploys = deploysOf(project)
  const prev = deploys[1]
  const thread = threadOf(project)
  const incident = project.state !== 'ok'
  const errValue = rolledBack ? '0.12' : project.err.toFixed(2)
  const currentDep = rolledBack && prev ? prev.id : project.dep

  const errState: State = rolledBack ? 'ok' : project.state
  const more = [
    ...(project.cert !== '—' && Number.parseInt(project.cert, 10) < 30 ? [{ state: 'warn' as State, children: <>Certificate for {project.domain} expires in {project.cert}. <Phrase onClick={() => setTab('settings')}>Renew</Phrase></> }] : []),
    ...(plan.sampled ? [{ state: 'sampled' as State, children: <>Telemetry is sampled 1 in 4 since 14:00.</> }] : []),
  ]
  const status = (
    <StatusLine state={errState} more={more.length ? { label: more.length > 1 ? `+${more.length} warnings` : '+1 warning', items: more } : undefined}>
      {rolledBack && prev ? <>Rolled back to {prev.id}, error rate recovering.</>
        : incident ? <><Phrase onClick={() => setTab('deploys')}>{project.note}</Phrase>. Roll back{prev ? <> to {prev.id}</> : null} or open the incident.</>
        : <>No open incidents. {project.dep} has been serving {project.env} for {project.deployed.replace(' ago', '')}.</>}
    </StatusLine>
  )
  // The one raised block on the page: what this project is doing right now, with the facts the reader
  // came for. Everything the lede says is not repeated in the meta, the sections or the thread.
  const lede = (
    <Lede state={rolledBack ? 'ok' : project.state} word={project.state === 'error' ? 'failing' : 'serving'}
      facts={[
        { k: 'domain', v: project.domain },
        { k: 'current deploy', v: currentDep },
        { k: 'branch', v: <Branch name={project.branch} /> },
        { k: 'replicas', v: project.replicas },
        { k: 'last deploy', v: project.deployed },
        { k: 'error rate · 24h', v: `${errValue}%`, state: Number(errValue) > 1 ? 'error' : Number(errValue) > 0.5 ? 'warn' : undefined },
      ]}>
      {project.env} · {project.visitors > 0 ? `${(project.visitors / 1000).toFixed(1)}k visitors in 24h` : 'no public traffic'}
    </Lede>
  )
  const rollback = (trigger: ReactNode) => (
    prev ? <EchoDialog trigger={trigger} echo={`$ temps deploy rollback ${project.name} --to ${prev.id}`} title="Roll back" description={`Routes ${project.env} traffic back to ${prev.id}. ${project.dep} stays available for inspection. About 5 seconds, no downtime.`} confirmWord={prev.id}
      steps={[`verify ${prev.id} image present`, `start ${prev.id} containers`, 'health check /healthz', 'switch proxy routes', `drain ${project.dep}`]}
      onDone={() => { setRolledBack(true); notify('ok', `rolled back to ${prev.id}`, `${project.name} · ${project.env} · 4.8s`) }} /> : null
  )
  /** An incident row opens the thing it names; a row with nothing behind it draws no link at all. */
  const openThread = (link: ThreadLink) => {
    if (link === 'deploy') go(`deploy:${project.dep}`)
    else if (link === 'error') go(project.state === 'error' ? 'issue:i_4830' : 'issue:i_4821')
    else if (link === 'replay') notify('ok', 'session replay', 'sess_9f31c · /checkout → submit → error · Safari 17 · 1:24')
    else if (link === 'alerts') go('metrics')
  }
  const LINK_LABEL: Record<Exclude<ThreadLink, null>, string> = { deploy: 'open the deploy', error: 'open the issue', replay: 'open the replay', alerts: 'open the alert' }

  if (tab === 'settings') {
    return (
      <Detail title={project.name} mark={<ProjectMark name={project.name} icon={project.icon} size={24} />} meta={`${project.kind} · github.com/acme/${project.repo}`} status={status} lede={lede} tabs={TABS} tab={tab} onTab={setTab}>
        <Settings
          status={null} dirty={dirty} onSave={() => { setSaved(form); notify('ok', 'settings saved', `${project.name} · branch ${form.branch} · ${form.domain}`) }}
          sections={[
            { title: 'deploy', body: <><Field label="auto-deploy branch" help="every push to this branch builds and deploys · branches from github.com/acme/api-gateway"><Picker value={form.branch} onChange={(b) => setForm({ ...form, branch: b })} options={BRANCHES} allowCustom="use branch" searchPlaceholder="filter 9 branches" /></Field><Field label="health check" help="must return 200 within 30s or the deploy is rolled back"><Input value={form.health} onChange={(e) => setForm({ ...form, health: e.target.value })} className="h-8 font-mono text-xs" /></Field></> },
            { title: 'domains', body: <Field label="production domain" help="certificate renews automatically 30 days before expiry · current cert expires in 6d"><Input value={form.domain} onChange={(e) => setForm({ ...form, domain: e.target.value })} className="h-8 font-mono text-xs" /></Field> },
            { title: 'telemetry', body: <div className="text-xs"><p>Retention <span className="font-mono">{plan.retention}</span> · ingest allowance <span className="font-mono">{plan.ingest ?? 'none (your disk)'}</span>{plan.sampled && <span className="text-muted-foreground"> · sampled 1 in 4 since 14:00</span>}</p><p className="mt-1 text-[11px] text-muted-foreground">{plan.id === 'selfhost' ? 'Self-hosted keeps whatever you configure. Nothing is metered.' : 'Past the allowance, telemetry is head-sampled and every chart says so. It is never silently dropped.'}</p></div> },
          ]}
          danger={
            <div className="flex flex-wrap items-center justify-between gap-3 text-xs">
              <div><p className="font-medium">Delete this project</p><p className="text-[11px] text-muted-foreground">Removes containers, routes and certificates. Database and backups are kept.</p></div>
              <EchoDialog trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> delete project</Button>} destructive echo={`$ temps project delete ${project.name} --keep-data`} title="Delete project" description="Stops api-gateway, removes its routes and certificate. Backups and the database stay and can be attached to a new project." confirmWord={project.name}
                steps={['stop containers', 'remove proxy routes', 'revoke certificate', 'archive project record']} onDone={() => { notify('warn', `${project.name} deleted`, 'database and backups kept'); go('projects') }} />
            </div>
          }
        />
      </Detail>
    )
  }

  return (
    <Detail title={project.name} mark={<ProjectMark name={project.name} icon={project.icon} size={24} />} meta={`${project.kind} · github.com/acme/${project.repo}`} status={status} lede={lede} tabs={TABS} tab={tab} onTab={setTab}
      actions={<>
        {tab === 'overview' && <Segmented options={[['none', 'today'], ['yesterday', 'vs yesterday']] as const} value={compare} onChange={setCompare} />}
        {rollback(<Button variant="outline" size="sm" className="h-8 text-xs" disabled={rolledBack}><RotateCcw /> roll back</Button>)}
        <Button ref={deployBtn} size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'deploy started', 'dep_92b · main@e4d1f0a')}><Rocket /> deploy <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button>
      </>}>
      {tab === 'overview' && (loading ? (
        <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]"><div className="space-y-6"><Skeleton className="h-52 w-full rounded-none" /><Skeleton className="h-24 w-full rounded-none" /></div><Skeleton className="h-80 w-full rounded-none" /></div>
      ) : (
        <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
          <div className="space-y-6">
            <RequestsChart hot={hot} onHot={setHot} compare={compare === 'yesterday'} deploys={deploys} />
            <MetricGrid cols={4}>
              <Metric label={project.kind === 'worker' ? 'jobs · 24h' : 'requests · 24h'} value={project.kind === 'worker' ? '18.4k' : `${(project.visitors / 1000).toFixed(1)}k`} delta={compare === 'yesterday' ? '+12%' : '+9%'} baseline={compare === 'yesterday' ? 'vs yesterday' : `since ${project.dep}`} />
              <Metric label="error rate" value={errValue} unit="%" delta={rolledBack ? '↓' : incident ? '+0.2pt' : '−0.01pt'} baseline={rolledBack ? 'since rollback' : `since ${project.dep}`} state={rolledBack ? 'ok' : Number(errValue) > 0.5 ? 'warn' : 'ok'} />
              <Metric label="p95 latency" value={184} unit="ms" delta="−9ms" baseline={prev ? `vs ${prev.id}` : 'first deploy'} />
              <Metric label="uptime · 90d" value="99.94" unit="%" baseline="2 incidents · 90d window" />
            </MetricGrid>
            <Section title="Recent deploys" meta={`${deploys.length} of 41 · all deploys in the deploys tab`}>
            <div className="op-rows border">
              <div className="op-row hidden items-center md:grid md:grid-cols-[80px_80px_1fr_100px]"><span className="op-label">deploy</span><span className="op-label">when</span><span className="op-label">commit</span><span className="op-label">build</span></div>
              {deploys.map((d) => (
                <div key={d.id} role="button" tabIndex={0} onClick={() => go(`deploy:${d.id}`)} onKeyDown={(e) => { if (e.key === 'Enter') go(`deploy:${d.id}`) }} onMouseEnter={() => setHot(d.id)} onMouseLeave={() => setHot(null)} className={cn('op-row grid cursor-pointer grid-cols-[80px_1fr] items-center gap-x-3 text-xs outline-none hover:bg-muted/60 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring md:grid-cols-[80px_80px_1fr_100px]', hot === d.id && 'op-marker-hot')}>
                  <span className="font-mono"><Status state={d.id === project.dep && rolledBack ? 'idle' : d.state} label={d.id} /></span>
                  <span className="text-muted-foreground">{d.at}</span>
                  <span className="col-span-2 truncate md:col-span-1"><span className="font-mono text-muted-foreground">{d.commit}</span> {d.msg}</span>
                  <span className="hidden md:block"><Num value={d.dur} /></span>
                </div>
              ))}
            </div>
            </Section>
          </div>
          <Section className="self-start" title={incident ? 'Incident' : 'Activity'} meta={`since ${project.dep}`}
            action={<Status state={rolledBack ? 'ok' : incident ? project.state : 'ok'} label={rolledBack ? 'mitigated · rolled back' : incident ? 'open · 6m' : 'nothing open'} />}>
          <div className={cn('border bg-background', rolledBack && 'opacity-80')}>
            <ol className="op-rows">
              {thread.map((e) => (
                <li key={e.at} className="grid grid-cols-[64px_1fr] gap-x-3 px-3 py-2 text-xs">
                  <span className="font-mono tabular-nums text-muted-foreground">{e.at}</span>
                  <span className="min-w-0">
                    {/* The state is a glyph and a word; the message stays ink so a long sentence wraps instead of being one long red line. */}
                    <span className="block min-w-0">{e.kind === 'error'
                      ? <span className="flex min-w-0 flex-wrap items-baseline gap-x-2"><Status state={project.state === 'error' ? 'error' : 'warn'} label="error" /><span className="min-w-0">{e.title}</span></span>
                      : e.title}</span>
                    <span className="block truncate text-[11px] text-muted-foreground">{e.detail}</span>
                    {e.link && <button type="button" onClick={() => openThread(e.link)} className="text-[11px] underline underline-offset-4 hover:text-foreground">{LINK_LABEL[e.link]}</button>}
                  </span>
                </li>
              ))}
              {rolledBack && prev && <li className="grid grid-cols-[64px_1fr] gap-x-3 px-3 py-2 text-xs"><span className="font-mono tabular-nums text-muted-foreground">now</span><span><span className="block">rolled back to {prev.id}</span><span className="block text-[11px] text-muted-foreground">by maya · 4.8s · {project.dep} kept for inspection</span></span></li>}
            </ol>
            <div className="flex flex-wrap items-center justify-between gap-2 border-t px-3 py-2">
              <span className="min-w-0 flex-1 text-[11px] text-muted-foreground">deploy, error, session and alert are one thread</span>
              {incident && !rolledBack && rollback(<Button size="sm" className="op-primary h-7 text-xs"><RotateCcw /> roll back</Button>)}
            </div>
          </div>
          </Section>
        </div>
      ))}

      {tab === 'deploys' && <DeploysTab notify={notify} dense={dense} go={go} />}
      {tab === 'environments' && <EnvironmentsTab notify={notify} dense={dense} />}
      {tab === 'variables' && <VariablesTab notify={notify} dense={dense} />}

      {tab === 'logs' && <LogViewer lines={lines} title="build · dep_91a" className="max-h-80" />}
    </Detail>
  )
}

// ── Palette ────────────────────────────────────────────────────────────

function Palette({ open, onOpenChange, go }: { open: boolean; onOpenChange: (o: boolean) => void; go: (v: string) => void }) {
  const notify = useNotify()
  const heading = '[&_[cmdk-group-heading]]:py-1 [&_[cmdk-group-heading]]:text-[10px] [&_[cmdk-group-heading]]:uppercase [&_[cmdk-group-heading]]:tracking-[0.1em]'
  const item = 'rounded-none data-[selected=true]:bg-foreground data-[selected=true]:text-background'
  return (
    // Anchored near the top, not centred: a centred dialog grows downward as results change and its tail leaves the viewport. The list scrolls inside a fixed height.
    <CommandDialog open={open} onOpenChange={onOpenChange} contentClassName={cn(SKIN, 'top-[8vh] max-h-[84vh] translate-y-0 border shadow-none data-[state=open]:slide-in-from-top-0 data-[state=closed]:slide-out-to-top-0 sm:rounded')}>
      <CommandInput prompt=">" placeholder="jump to a project, or run a command…" className="font-mono text-xs" />
      <CommandList className="max-h-[min(70vh,640px)] font-mono text-xs">
        <CommandEmpty>no matches</CommandEmpty>
        <CommandGroup heading="projects" className={heading}>
          {PROJECTS.map((p) => <CommandItem key={p.name} onSelect={() => { onOpenChange(false); go(p.name) }} className={item}><Status state={p.state} label="" /><ProjectMark name={p.name} icon={p.icon} />{p.name}<CommandShortcut className="text-inherit opacity-60">{p.env}</CommandShortcut></CommandItem>)}
        </CommandGroup>
        {/* Pages carry the same icon as the sidebar, so the palette and the nav read as one map. Commands carry the icon of what they do. */}
        <CommandGroup heading="pages" className={heading}>
          {NAV.flatMap((g) => g.items.filter((it) => it[2]).map(([l, Icon, v]) => (
            <CommandItem key={v} value={`${l} ${g.group}`} onSelect={() => { onOpenChange(false); go(v) }} className={item}>
              <Icon className="h-3.5 w-3.5 opacity-70" /><span>{l.toLowerCase()}</span><CommandShortcut className="text-inherit opacity-60">{g.group}</CommandShortcut>
            </CommandItem>
          )))}
        </CommandGroup>
        <CommandGroup heading="commands" className={heading}>
          {([['deploy api-gateway', `${MOD} ⏎`, Rocket], ['open build logs', '', ScrollText], ['create backup now', '', HardDrive], ['toggle density', 'd', Rows3]] as const).map(([l, k, Icon]) => (
            <CommandItem key={l} onSelect={() => { onOpenChange(false); notify('ok', l) }} className={item}>
              <Icon className="h-3.5 w-3.5 opacity-70" /><span>{l}</span>{k && <CommandShortcut className="text-inherit opacity-60">{k}</CommandShortcut>}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  )
}

// ── Shell ──────────────────────────────────────────────────────────────

const NAV = [
  { group: 'platform', items: [['Projects', FolderOpen, 'projects'], ['Sandboxes', Terminal, 'sandboxes'], ['Email', Mail, 'email']] },
  { group: 'observe', items: [['Analytics', BarChart3, 'analytics'], ['Errors', Activity, 'errors'], ['Traces', Waypoints, 'traces'], ['Metrics', Gauge, 'metrics'], ['Uptime', Globe, 'uptime'], ['Proxy', ArrowLeftRight, 'proxy']] },
  { group: 'storage', items: [['Databases', Database, 'databases'], ['Backups', HardDrive, 'backups']] },
  { group: 'source', items: [['Git providers', GitBranch, 'git'], ['Security', ShieldCheck, 'security']] },
  { group: 'instance', items: [['Settings', Cog, 'settings']] },
] as const
const PAGES = ['projects', 'databases', 'errors', 'analytics', 'sandboxes', 'traces', 'metrics', 'backups', 'git', 'security', 'email', 'uptime', 'proxy', 'settings']
/** Which nav page a view belongs to: 'sandbox:sbx_1' → 'sandboxes', 'trace:…' → 'traces', 'api-gateway' → 'projects'. */
function pageOf(view: string) {
  if (PAGES.includes(view)) return view
  if (view.startsWith('sandbox:')) return 'sandboxes'
  if (view.startsWith('trace:')) return 'traces'
  if (view.startsWith('git:')) return 'git'
  if (view.startsWith('scan:')) return 'security'
  if (view.startsWith('email:') || view.startsWith('domain:')) return 'email'
  if (view.startsWith('event:')) return 'analytics'
  if (view.startsWith('deploy:')) return 'projects'
  if (view.startsWith('db:')) return 'databases'
  if (view.startsWith('issue:')) return 'errors'
  if (view.startsWith('monitor:')) return 'uptime'
  if (view.startsWith('settings:') || view === 'nodes' || view.startsWith('node:')) return 'settings'
  return 'projects'
}

/** Nav group a page belongs to, e.g. 'security' → 'source'. */
function navGroup(page: string) {
  return NAV.find((g) => g.items.some((it) => it[2] === page))?.group ?? 'platform'
}
function navLabel(page: string) {
  for (const g of NAV) for (const it of g.items) if (it[2] === page) return it[0]
  return page
}

/* Observe screens need the plan (retention, sampling); the provider lives in
   the shell, so they render through this child. */
function ObserveRoutes({ view, go, dense }: { view: string; go: (v: string) => void; dense: boolean }) {
  const plan = usePlan()
  const notify = useNotify()
  if (view === 'sandboxes') return <SandboxesScreen go={go} dense={dense} />
  if (view.startsWith('deploy:')) return <DeploymentScreen tag={view.slice(7)} dense={dense} notify={notify} go={go} />
  if (view.startsWith('sandbox:')) return <SandboxScreen id={view.slice(8)} notify={notify} dense={dense} go={go} />
  if (view === 'traces') return <TracesScreen go={go} dense={dense} plan={plan} notify={notify} />
  if (view.startsWith('trace:')) return <TraceScreen id={view.slice(6)} go={go} dense={dense} />
  if (view === 'metrics') return <MetricsScreen dense={dense} plan={plan} notify={notify} />
  if (view === 'backups') return <BackupsScreen dense={dense} plan={plan} notify={notify} go={go} />
  if (view === 'git') return <GitProvidersScreen dense={dense} go={go} />
  if (view.startsWith('git:')) return <GitProviderScreen id={view.slice(4)} dense={dense} notify={notify} go={go} />
  if (view === 'security') return <SecurityScreen dense={dense} notify={notify} go={go} />
  if (view === 'analytics') return <AnalyticsScreen dense={dense} plan={plan} notify={notify} go={go} />
  if (view.startsWith('event:')) return <EventScreen name={view.slice(6)} go={go} />
  if (view === 'uptime') return <UptimeScreen dense={dense} notify={notify} go={go} />
  if (view.startsWith('monitor:')) return <MonitorScreen id={view.slice(8)} notify={notify} go={go} />
  if (view === 'settings') return <SettingsHub go={go} />
  if (view === 'nodes') return <SettingsPage slug="nodes" dense={dense} notify={notify} go={go} />
  if (view.startsWith('node:')) return <NodeScreen name={view.slice(5)} dense={dense} notify={notify} go={go} />
  if (view.startsWith('settings:')) return <SettingsPage slug={view.slice(9)} dense={dense} notify={notify} go={go} />
  if (view === 'proxy') return <ProxyScreen dense={dense} plan={plan} notify={notify} go={go} />
  if (view === 'errors') return <ErrorsScreen dense={dense} plan={plan} notify={notify} go={go} />
  if (view.startsWith('issue:')) return <IssueScreen id={view.slice(6)} dense={dense} notify={notify} go={go} />
  if (view.startsWith('db:')) return <DatabaseScreen id={view.slice(3)} dense={dense} notify={notify} go={go} />
  if (view === 'email') return <EmailScreen dense={dense} notify={notify} go={go} />
  if (view.startsWith('email:')) return <EmailDetailScreen id={view.slice(6)} dense={dense} notify={notify} go={go} />
  if (view.startsWith('domain:')) return <EmailDomainScreen id={view.slice(7)} dense={dense} notify={notify} go={go} />
  if (view.startsWith('scan:')) return <ScanScreen id={view.slice(5)} dense={dense} go={go} notify={notify} />
  return null
}

const DENSITY_KEY = 'temps.ds.v5.density'

export function ConsoleV5({ view, go, fullHref, full }: { view: string; go: (v: string) => void; /** Where the ⤢ button goes: the chrome-free route (or back to the sandbox page when already full). */ fullHref?: string; full?: boolean }) {
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [navOpen, setNavOpen] = useState(false)
  const [notesOpen, setNotesOpen] = useState(false)
  const [plan, setPlan] = useState<PlanId>('selfhost')
  const [fresh, setFresh] = useFresh()
  // Density: comfortable by default, remembered. Operators turn it on once.
  const [dense, setDenseState] = useState(() => typeof localStorage !== 'undefined' && localStorage.getItem(DENSITY_KEY) === 'dense')
  const setDense = useCallback((f: (d: boolean) => boolean) => setDenseState((d) => { const n = f(d); localStorage.setItem(DENSITY_KEY, n ? 'dense' : 'comfortable'); return n }), [])
  const [notes, setNotes] = useState<Note[]>([])
  const push = useCallback((n: Omit<Note, 'id' | 'ts'>) => {
    const note: Note = { ...n, id: Date.now() + Math.random(), ts: new Date().toISOString().slice(11, 19) }
    setNotes((prev) => [note, ...prev].slice(0, 50))
    toast.custom((id) => <div className={cn(SKIN, 'w-[min(360px,calc(100vw-2rem))] border bg-background px-3 py-2')}><NoteRow n={note} onUndo={() => toast.dismiss(id)} /></div>, { position: 'bottom-left', duration: note.undo ? 8000 : 5000 })
  }, [])

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); setPaletteOpen((o) => !o); return }
      if (tag === 'INPUT' || tag === 'TEXTAREA' || e.metaKey || e.ctrlKey) return
      if (e.key === 'd') setDense((d) => !d)
      if (e.key === 'Escape') { setNotesOpen(false); setNavOpen(false) }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [setDense])

  const unread = notes.filter((n) => n.level !== 'ok').length
  const page = pageOf(view)
  // Header slots: the screen's PageTitle fills the last crumb, its StatusLine fills the attention indicator.
  const [crumbSlot, setCrumbSlot] = useState<HTMLElement | null>(null)
  const bellWrap = useRef<HTMLDivElement>(null)
  const [attentionSlot, setAttentionSlot] = useState<HTMLElement | null>(null)
  const active = (target: string) => target === page

  return (
    <NotesContext.Provider value={{ notes, push }}>
      <PlanContext.Provider value={{ plan, setPlan }}>
        <div data-density={dense ? 'dense' : 'comfortable'} className={cn(SKIN, 'relative flex flex-1')}>
          <Palette open={paletteOpen} onOpenChange={setPaletteOpen} go={go} />
          {navOpen && <button aria-label="Close menu" className="fixed inset-0 z-30 bg-foreground/20 lg:hidden" onClick={() => setNavOpen(false)} />}
          <aside className={cn('relative w-52 shrink-0 border-r bg-background', navOpen ? 'fixed inset-y-0 left-0 z-40 block' : 'hidden lg:block')}>
            <div className="flex h-11 items-center gap-2 border-b px-3">
              <LogoMark size={18} /><span className="text-sm font-semibold">temps</span>
              <span className="ml-auto font-mono text-[10px] text-muted-foreground">v0.1.0</span>
              <button className="lg:hidden" onClick={() => setNavOpen(false)} aria-label="Close"><X className="h-4 w-4" /></button>
            </div>
            <nav className="py-2 text-xs">
              {NAV.map((g) => (
                <div key={g.group} className="mb-2">
                  <p className="op-label px-3 py-1">{g.group}</p>
                  {g.items.map(([label, Icon, target]) => (
                    <button key={label} type="button" onClick={() => { if (target) go(target); setNavOpen(false) }} className={cn('flex h-7 w-full items-center gap-2 px-3 text-left hover:bg-muted focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring', target && active(target) && 'border-l-2 border-foreground bg-muted pl-[10px] font-medium', !target && 'text-muted-foreground')}>
                      <Icon className="h-3.5 w-3.5" /> {label}
                    </button>
                  ))}
                </div>
              ))}
            </nav>
            <div className="absolute inset-x-0 bottom-0 hidden border-t p-3 text-[11px] text-muted-foreground lg:block">
              <Kbd keys="d" className="mr-1" /> density · <Kbd keys={['⌘', 'K']} className="mx-1" /> find
            </div>
          </aside>

          <div className="flex min-w-0 flex-1 flex-col">
            <header className="flex h-11 items-center gap-2 border-b px-3 text-xs sm:px-4">
              <button type="button" className="flex h-7 w-7 items-center justify-center border lg:hidden" onClick={() => setNavOpen(true)} aria-label="Menu"><Menu className="h-4 w-4" /></button>
              <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1.5 truncate text-muted-foreground">
                <span className="hidden sm:inline">{navGroup(page)}</span>
                <span aria-hidden className="hidden text-[var(--op-rule-soft)] sm:inline">/</span>
                {PAGES.includes(view)
                  ? null
                  : <><a href="#" onClick={(e) => { e.preventDefault(); go(page) }} className="truncate hover:text-foreground">{navLabel(page)}</a></>}
                <span ref={setCrumbSlot} className={cn('flex min-w-0 items-center gap-1.5 truncate', PAGES.includes(view) && '[&>span:first-child]:hidden')} />
              </nav>
              <div className="ml-auto flex shrink-0 items-center gap-2">
                <AttentionHost onSlot={setAttentionSlot} />
                {/* Sandbox-only knobs: below xl the sidebar plus the real controls fill the header, so these go before anything a user would need. */}
                <label className="hidden items-center gap-1 text-[11px] text-muted-foreground xl:flex" title="Render every screen as a console that was installed minutes ago: nothing configured, nothing recorded.">
                  <input type="checkbox" checked={fresh} onChange={(e) => setFresh(e.target.checked)} className="accent-foreground" aria-label="Fresh install (demo)" /> fresh
                </label>
                <label className="hidden items-center gap-1 text-[11px] text-muted-foreground xl:flex">
                  plan
                  <select value={plan} onChange={(e) => setPlan(e.target.value as PlanId)} className="h-7 border bg-background px-1 font-mono text-[11px] text-foreground" aria-label="Plan (demo)">
                    {(Object.keys(PLANS) as PlanId[]).map((k) => <option key={k} value={k}>{PLANS[k].label}</option>)}
                  </select>
                </label>
                <Button variant="outline" size="sm" className="h-7 text-xs" onClick={() => setPaletteOpen(true)}><Search /> <span className="hidden sm:inline">find</span> <Kbd keys={['⌘', 'K']} className="ml-1" /></Button>
                <Button variant="outline" size="icon" className={cn('h-7 w-7', dense && 'bg-foreground text-background')} aria-pressed={dense} aria-label="Toggle density" onClick={() => setDense((d) => !d)}><Rows3 /></Button>
                {fullHref && <Button variant="outline" size="icon" className="h-7 w-7" asChild><Link to={fullHref} aria-label={full ? 'Exit full screen' : 'Full screen'} title={full ? 'back to the sandbox page' : 'the console alone, no sandbox chrome'}>{full ? <Minimize2 /> : <Maximize2 />}</Link></Button>}
                <div className="relative" ref={bellWrap}>
                  <Button variant="outline" size="icon" className="h-7 w-7" aria-label="Notifications" aria-expanded={notesOpen} onClick={() => setNotesOpen((o) => !o)}><Bell /></Button>
                  {unread > 0 && <span className="absolute -right-1 -top-1 flex h-3.5 min-w-3.5 items-center justify-center bg-foreground px-0.5 font-mono text-[9px] text-background">{unread}</span>}
                  <Drop anchor={bellWrap} open={notesOpen} width={360}>
                      <div className="flex items-center justify-between border-b px-3 py-2"><span className="op-label">notifications</span><span className="text-[11px] text-muted-foreground">{notes.length} · newest first</span></div>
                      <div className="op-rows max-h-72 overflow-auto">
                        {notes.length === 0 ? <p className="px-3 py-4 text-[11px] text-muted-foreground">Nothing yet. Deploys, rollbacks and alerts land here and as toasts.</p> : notes.map((n) => <div key={n.id} className="px-3 py-2"><NoteRow n={n} /></div>)}
                      </div>
                  </Drop>
                </div>
                <span className="hidden items-center gap-2 border-l pl-3 sm:flex"><span className="flex h-5 w-5 items-center justify-center border text-[10px]">M</span><span className="text-muted-foreground">maya</span></span>
              </div>
            </header>
            <main className="flex-1 px-4 pb-4 sm:px-6 sm:pb-6">
             <ShellSlotsProvider value={{ crumb: crumbSlot, attention: attentionSlot }}>
              {view === 'projects' && <ProjectsScreen go={go} dense={dense} />}
              {view === 'databases' && <DatabasesScreen dense={dense} go={go} />}
              {(page === 'sandboxes' || page === 'traces' || page === 'metrics' || page === 'backups' || page === 'git' || page === 'security' || page === 'email' || page === 'analytics' || page === 'uptime' || page === 'proxy' || page === 'settings' || page === 'errors' || view.startsWith('db:') || view.startsWith('deploy:')) && <ObserveRoutes view={view} go={go} dense={dense} />}
              {page === 'projects' && view !== 'projects' && !view.startsWith('deploy:') && <ProjectScreen name={view} dense={dense} go={go} />}
            </ShellSlotsProvider>
            </main>
          </div>
        </div>
      </PlanContext.Provider>
    </NotesContext.Provider>
  )
}

export function ConsoleV5Page({ full = false }: { /** Render without the sandbox's layout and intro: the console as it would ship. Route `/console`. */ full?: boolean }) {
  const [params, setParams] = useSearchParams()
  const view = params.get('p') ?? 'projects'
  const go = useCallback((v: string) => {
    const p = new URLSearchParams(params)
    if (v === 'projects') p.delete('p')
    else p.set('p', v)
    setParams(p)
  }, [params, setParams])
  const search = params.toString() ? `?${params.toString()}` : ''
  const fullHref = (full ? '/v5' : '/console') + search
  return (
    <div className={cn('operator ink v4 v5 flex flex-col', full ? 'min-h-screen' : '-m-4 min-h-[calc(100vh-4.5rem)] sm:-m-6 lg:-m-8')}>
      {!full && <div className="border-b px-4 py-3 text-xs sm:px-6">
        <p className="op-label">operator console · v5 · the twelve answers as code</p>
        <p className="op-prose mt-1 max-w-3xl text-sm text-muted-foreground">
          Every screen is one of three templates: ledger (<a href="?p=projects" className="underline underline-offset-4">projects</a>, <a href="?p=databases" className="underline underline-offset-4">databases</a>, <a href="?p=errors" className="underline underline-offset-4">errors</a>), detail (<a href="?p=api-gateway" className="underline underline-offset-4">api-gateway</a>, with deploys + promote, environments, and per-environment variables as tabs 2–4), settings (tab 6 there, <Kbd keys={['⌘', 'S']} /> saves). One <span className="font-mono">PageState</span> with four states: loading, empty, unconfigured (<a href="?p=analytics" className="underline underline-offset-4">analytics</a>, <a href="?p=acme-web" className="underline underline-offset-4">acme-web</a>), error (<a href="?p=errors" className="underline underline-offset-4">errors</a>, with retry). A fifth status, <span className="font-mono">◌ sampled</span>, from the pricing promise. Retention horizon on every axis. Metric tiles must name their baseline. Sandboxes (<a href="?p=sandboxes" className="underline underline-offset-4">list</a>, <a href="?p=sandbox:sbx_7f21" className="underline underline-offset-4">detail</a>, <a href="?p=sandbox:sbx_e77b" className="underline underline-offset-4">failed</a>), <a href="?p=traces" className="underline underline-offset-4">traces</a> with operations, a <a href="?p=trace:3f9c1e7a8b2d4f60" className="underline underline-offset-4">trace waterfall</a>, and the <a href="?p=metrics" className="underline underline-offset-4">metrics explorer</a>. Backups (<a href="?p=backups" className="underline underline-offset-4">schedules, jobs, sources</a>), <a href="?p=git" className="underline underline-offset-4">git providers</a> with an expired installation, and <a href="?p=security" className="underline underline-offset-4">security</a> (scans, headers, access). Switch the plan in the header to see the same screens on self-hosted, Starter, Team and Business. No accent, radius frozen, density remembered.
        </p>
      </div>}
      <ConsoleV5 view={view} go={go} fullHref={fullHref} full={full} />
    </div>
  )
}

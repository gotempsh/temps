// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type CSSProperties } from 'react'
import { ArrowRight, Check, Plus, RotateCcw, Search, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Block, Demo, DocPage, Rule } from '@/components/op-doc'
import {
  ChartFooter,
  Detail,
  EchoDialog,
  Field,
  Kbd,
  Ledger,
  Metric,
  MetricGrid,
  Num,
  PageState,
  Phrase,
  Picker,
  RangePicker,
  SecretValue,
  Segmented,
  Settings,
  Status,
  StatusLine,
  TimeChart,
  worst,
  STATE_RANK,
  type LedgerRow,
  type PickerOption,
  type Range,
  type State,
  type TimePoint,
} from '@/components/op'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   /patterns — the PAGE patterns of v5. Where /op-components documents one
   component at a time, this page documents the shapes a whole screen can
   take: the three templates, the four non-happy states, and the four
   cross-cutting patterns that every screen has to get right (promote and
   roll back, per-environment variables, time and retention, keyboard).

   Every example here is live and built from src/components/op. Nothing is
   a screenshot and nothing is a placeholder box: if an example cannot be
   made to work, the pattern is not ready to be documented.

   Data is copied from ConsoleV5*.tsx rather than imported, because those
   files carry page state and hooks. Only the plain constants travel.
   ──────────────────────────────────────────────────────────────────────── */

const TOC = [
  ['ledger', 'Ledger'],
  ['detail', 'Detail'],
  ['settings', 'Settings'],
  ['states', 'The four states'],
  ['promote', 'Promote · roll back'],
  ['variables', 'Variables per env'],
  ['time', 'Time · retention'],
  ['keyboard', 'Keyboard'],
  ['responsive', 'Responsive'],
] as const

// ── Shared data (copied, not imported) ─────────────────────────────────

const BRANCHES: PickerOption[] = [
  { value: 'main', group: 'default', meta: 'e4d1f0a · 41m ago', keywords: 'master trunk' },
  { value: 'staging', group: 'recent', meta: '9bc61c0 · 2h ago' },
  { value: 'feat/checkout-address', group: 'recent', meta: 'b7c9d21 · 6h ago', keywords: '212' },
  { value: 'feat/rate-limits', group: 'recent', meta: '7e1c2aa · 3h ago' },
  { value: 'fix/edge-cache', group: 'recent', meta: 'c0ffee1 · yesterday' },
]

/** 24 half-hourly points, deterministic so the page never flickers. */
const CHART: TimePoint[] = Array.from({ length: 24 }, (_, i) => {
  const h = i
  const day = Math.max(0, Math.sin(((h - 7) / 24) * Math.PI * 2)) ** 1.2
  return {
    t: `${String(h).padStart(2, '0')}:00`,
    req: Math.round(1100 * (0.25 + day)) + (h >= 20 ? (h - 19) * 90 : 0),
  }
})
const CHART_MARKERS = [
  { id: 'dep_90e', x: '10:00' },
  { id: 'dep_91a', x: '20:00' },
]

// ── 1. Ledger ──────────────────────────────────────────────────────────

type Db = { name: string; engine: string; state: State; size: string; backup: string; pitr: string | null; note: string }
const DATABASES: Db[] = [
  { name: 'acme-pg', engine: 'PostgreSQL 18', state: 'ok', size: '4.2 GB', backup: '2h ago', pitr: '7d', note: 'healthy' },
  { name: 'billing-maria', engine: 'MariaDB 11', state: 'ok', size: '1.1 GB', backup: '2h ago', pitr: '7d', note: 'healthy' },
  { name: 'events-ch', engine: 'ClickHouse 24', state: 'warn', size: '38 GB', backup: '3d ago', pitr: null, note: 'backup older than 24h' },
  { name: 'sessions-redis', engine: 'Redis 7', state: 'ok', size: '180 MB', backup: '2h ago', pitr: null, note: 'healthy' },
  { name: 'catalog-mongo', engine: 'MongoDB 7', state: 'ok', size: '6.8 GB', backup: '2h ago', pitr: '7d', note: 'healthy' },
]
const DB_GRID = '1.3fr 1.2fr 90px 110px 90px'

function LedgerExample() {
  const [q, setQ] = useState('')
  const [opened, setOpened] = useState<string | null>(null)

  const sorted = useMemo(
    () => [...DATABASES].sort((a, b) => STATE_RANK[a.state] - STATE_RANK[b.state] || a.name.localeCompare(b.name)),
    [],
  )
  const shown = sorted.filter((d) => d.name.includes(q) || d.engine.toLowerCase().includes(q.toLowerCase()))
  const attention = sorted.filter((d) => d.state !== 'ok').length
  const rows: LedgerRow[] = shown.map((d) => ({
    id: d.name,
    sort: { name: d.name, size: Number(d.size.split(' ')[0]) * (d.size.endsWith('GB') ? 1024 : 1), backup: d.backup === '3d ago' ? 4320 : 120, pitr: d.pitr ? 7 : null },
    state: d.state,
    onOpen: () => setOpened(d.name),
    mobile: (
      <span className="min-w-0">
        <span className="block truncate font-medium">{d.name}</span>
        <span className="block truncate text-[11px] text-muted-foreground">{d.engine} · {d.size} · backup {d.backup}</span>
      </span>
    ),
    cells: [
      <span className="font-medium">{d.name}</span>,
      <Status state={d.state} label={d.state === 'ok' ? d.engine : d.note} />,
      <Num value={d.size} />,
      <span className="text-muted-foreground">{d.backup}</span>,
      d.pitr ? <Num value={d.pitr} /> : <Num value={null} />,
    ],
  }))

  return (
    <Ledger
      title="Databases"
      meta="5 managed · acme · fsn1"
      dense={false}
      grid={DB_GRID}
      columns={[{ label: 'name', key: 'name' }, 'state', { label: 'size', key: 'size', numeric: true }, { label: 'last backup', key: 'backup' }, { label: 'pitr', key: 'pitr', numeric: true }]}
      rows={rows}
      total={DATABASES.length}
      filter={q}
      onFilter={setQ}
      placeholder="filter databases"
      hint="needs attention first, then name"
      action={<Button size="sm" className="op-primary h-8 text-xs"><Plus /> new database</Button>}
      status={
        <StatusLine sticky={false} state={worst(DATABASES.map((d) => d.state))}>
          <Phrase onClick={() => setQ('events-ch')}>events-ch</Phrase> has not been backed up in 3 days.
        </StatusLine>
      }
      footer={
        <>
          {shown.length} of {DATABASES.length} · {attention} needs attention · <Kbd keys="j" className="mx-1" />
          <Kbd keys="k" className="mr-1" /> move · <Kbd keys="⏎" className="mx-1" /> open{opened && <> · opened <span className="font-mono">{opened}</span></>}
        </>
      }
    />
  )
}

// ── 2. Detail ──────────────────────────────────────────────────────────

const DETAIL_TABS = ['overview', 'deploys', 'settings'] as const
type DetailTab = (typeof DETAIL_TABS)[number]

const DETAIL_DEPLOYS = [
  { id: 'dep_91a', at: '41m ago', commit: '9bc61c0', msg: 'feat(checkout): new address form', state: 'warn' as State, current: true },
  { id: 'dep_90e', at: '10h ago', commit: '4f21a8d', msg: 'perf(router): cache edge lookups', state: 'ok' as State, current: false },
  { id: 'dep_88c', at: 'yesterday', commit: 'c0ffee1', msg: 'chore: bump deps', state: 'ok' as State, current: false },
]

function DetailExample() {
  const [tab, setTab] = useState<DetailTab>('overview')
  const [hot, setHot] = useState<string | null>(null)
  const [seg, setSeg] = useState<'today' | 'yesterday'>('today')
  const [current, setCurrent] = useState('dep_91a')

  return (
    <Detail
      title="api-gateway"
      meta={`production · ${current} · main`}
      tabs={DETAIL_TABS}
      tab={tab}
      onTab={setTab}
      status={
        <StatusLine sticky={false} state="warn" more={{ label: '+1 warning', onClick: () => setTab('deploys') }}>
          Error rate is 0.61% since <Phrase onClick={() => { setTab('deploys'); setHot('dep_91a') }}>dep_91a</Phrase>.
        </StatusLine>
      }
      actions={
        <>
          <Segmented options={[['today', 'today'], ['yesterday', 'vs yesterday']] as const} value={seg} onChange={setSeg} />
          <Button size="sm" className="op-primary h-8 text-xs">deploy <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button>
        </>
      }
    >
      {tab === 'overview' && (
        <div className="space-y-4">
          <TimeChart
            data={CHART}
            series={[{ key: 'req', name: 'requests' }]}
            markers={CHART_MARKERS}
            hot={hot}
            onHot={setHot}
            unit="req"
            height={112}
            yTicks={[0, 1000, 2000]}
            xInterval={5}
          />
          <ChartFooter><span>requests / hour · 24h</span><span>· ┆ deploy</span><span>· retention 90d</span></ChartFooter>
          <MetricGrid cols={4}>
            <Metric label="requests" value="30.8k" baseline="24h window" />
            <Metric label="error rate" value="0.61" unit="%" delta="+0.42pt" baseline="since dep_91a" state="warn" />
            <Metric label="p95" value="184" unit="ms" delta="+31ms" baseline="since dep_91a" state="warn" />
            <Metric label="uptime" value="99.94" unit="%" baseline="30d window" />
          </MetricGrid>
          <div className="op-raise">
            <div className="border-b px-3 py-2"><p className="op-label">incident thread · the one raised element</p></div>
            <div className="op-rows text-xs">
              <div className="px-3 py-2">
                <p className="font-mono text-[11px] text-muted-foreground">20:38:41</p>
                <p className="truncate">TypeError: cannot read properties of undefined (reading 'id')</p>
                <p className="truncate text-muted-foreground">src/checkout/AddressForm.tsx:88 · 31 events · 12 users</p>
              </div>
              <div className="px-3 py-2">
                <p className="font-mono text-[11px] text-muted-foreground">20:40:00</p>
                <p className="truncate">error rate crossed 0.5%</p>
                <p className="truncate text-muted-foreground">now 0.61% · notified #ops</p>
              </div>
            </div>
          </div>
        </div>
      )}

      {tab === 'deploys' && (
        <div className="op-rows border text-xs">
          {DETAIL_DEPLOYS.map((d) => (
            <div
              key={d.id}
              onMouseEnter={() => setHot(d.id)}
              onMouseLeave={() => setHot(null)}
              className={cn('op-row flex flex-wrap items-center gap-x-3 gap-y-1', hot === d.id && 'op-marker-hot')}
            >
              <Status state={d.state} label={d.id} className="font-mono" />
              <span className="min-w-0 flex-1 truncate text-muted-foreground">{d.commit} · {d.msg}</span>
              <span className="hidden text-muted-foreground sm:inline">{d.at}</span>
              {d.id === current ? (
                <span className="border px-1 font-mono text-[10px] text-muted-foreground">current</span>
              ) : (
                <EchoDialog
                  echo={`$ temps deploy rollback --to ${d.id}`}
                  title={`Roll back to ${d.id}`}
                  description={`Re-points production at the image already built for ${d.id} (${d.commit}). No rebuild, about 20 seconds, no downtime.`}
                  confirmWord={d.id}
                  steps={[`verify ${d.id} image present`, 'render production variables', 'start containers', 'health check /healthz', 'switch proxy routes']}
                  onDone={() => setCurrent(d.id)}
                  trigger={<Button size="sm" variant="outline" className="h-6 px-2 text-[11px]"><RotateCcw /> roll back</Button>}
                />
              )}
            </div>
          ))}
        </div>
      )}

      {tab === 'settings' && (
        <p className="op-prose border p-4 text-xs text-muted-foreground">
          A settings tab inside a Detail is the Settings template's section list, not a third layout. See the block below.
        </p>
      )}
    </Detail>
  )
}

// ── 3. Settings ────────────────────────────────────────────────────────

type SettingsForm = { branch: string; command: string; alerts: boolean; digest: boolean }
const SETTINGS0: SettingsForm = { branch: 'main', command: 'cargo build --release', alerts: true, digest: false }

function SettingsExample() {
  const [form, setForm] = useState(SETTINGS0)
  const [saved, setSaved] = useState(SETTINGS0)
  const [deleted, setDeleted] = useState(false)
  const dirty = JSON.stringify(form) !== JSON.stringify(saved)
  const set = <K extends keyof SettingsForm>(k: K, v: SettingsForm[K]) => setForm((f) => ({ ...f, [k]: v }))

  return (
    <Settings
      title="api-gateway settings"
      meta="production · acme"
      dirty={dirty}
      onSave={() => setSaved(form)}
      status={
        <StatusLine sticky={false} state={dirty ? 'warn' : 'ok'}>
          {dirty ? <>Unsaved changes. <Phrase onClick={() => setForm(saved)}>Discard</Phrase>.</> : <>Saved. Deploys follow <span className="font-mono">{saved.branch}</span>.</>}
        </StatusLine>
      }
      sections={[
        {
          title: 'source',
          body: (
            <>
              <Field label="branch" help="Deploys are triggered by pushes to this branch.">
                <Picker value={form.branch} onChange={(v) => set('branch', v)} options={BRANCHES} allowCustom="use branch" />
              </Field>
              <Field label="build command" help="Run in the repository root inside the build container.">
                <Input value={form.command} onChange={(e) => set('command', e.target.value)} className="h-8 font-mono text-xs" />
              </Field>
            </>
          ),
        },
        {
          title: 'alerts',
          body: (
            <>
              <Field label="error rate" help="Notifies #ops when the rate crosses 0.5% for 5 minutes.">
                <span className="flex items-center gap-2">
                  <Switch checked={form.alerts} onCheckedChange={(v) => set('alerts', v)} />
                  <span className="text-xs text-muted-foreground">{form.alerts ? 'on' : 'off'}</span>
                </span>
              </Field>
              <Field label="weekly digest" help="One email on Monday with deploys, errors and ingest.">
                <span className="flex items-center gap-2">
                  <Checkbox checked={form.digest} onCheckedChange={(v) => set('digest', v === true)} />
                  <span className="text-xs text-muted-foreground">send to maya</span>
                </span>
              </Field>
            </>
          ),
        },
      ]}
      danger={
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <span className="op-prose min-w-0 flex-1 text-muted-foreground">
            {deleted ? 'api-gateway was deleted in this demo. Reload the page to bring it back.' : 'Deletes containers, domains, variables and telemetry. Backups are kept for 7 days.'}
          </span>
          <EchoDialog
            destructive
            echo="$ temps project delete api-gateway"
            title="Delete project"
            description="Removes every environment, container, domain and variable. Telemetry is dropped immediately; database backups are kept for 7 days."
            confirmWord="api-gateway"
            steps={['stop containers', 'release domains', 'delete variables', 'drop telemetry', 'delete project']}
            onDone={() => setDeleted(true)}
            trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> delete project</Button>}
          />
        </div>
      }
    />
  )
}

// ── 5. Promote and roll back ───────────────────────────────────────────

type EnvCell = { name: string; branch: string; url: string; protectedEnv: boolean }
const PROMO_ENVS: EnvCell[] = [
  { name: 'pr-212', branch: 'feat/rate-limits', url: 'pr-212-api.acme.sh', protectedEnv: false },
  { name: 'staging', branch: 'develop', url: 'staging-api.acme.sh', protectedEnv: false },
  { name: 'production', branch: 'main', url: 'api.acme.sh', protectedEnv: true },
]
type Dep = { tag: string; commit: string; msg: string; at: string }
const PREVIEW_DEP: Dep = { tag: 'dep_94b', commit: '7e1c2aa', msg: 'feat(api): per-key rate limits', at: '3h ago' }
const STAGING_DEP: Dep = { tag: 'dep_93c', commit: 'd41f9e0', msg: 'fix(checkout): address form null id', at: '18m ago' }
const PROD_DEP: Dep = { tag: 'dep_91a', commit: '9bc61c0', msg: 'feat(checkout): new address form', at: '41m ago' }
const PREV_PROD_DEP: Dep = { tag: 'dep_90e', commit: '4f21a8d', msg: 'perf(router): cache edge lookups', at: '10h ago' }

function EnvPane({ env, dep }: { env: EnvCell; dep: Dep }) {
  return (
    <div className="min-w-0 bg-background p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="op-label truncate">{env.name}</span>
        <Status state="ok" label={env.protectedEnv ? 'protected' : env.branch} className="text-[11px]" />
      </div>
      <p className="mt-1 truncate font-mono text-sm">{dep.tag} <span className="text-muted-foreground">{dep.commit}</span></p>
      <p className="truncate text-[11px]">{dep.msg}</p>
      <p className="truncate font-mono text-[11px] text-muted-foreground">{dep.at} · {env.url}</p>
    </div>
  )
}

function PromoteExample() {
  const [prod, setProd] = useState(PROD_DEP)
  const inSync = prod.commit === STAGING_DEP.commit

  return (
    <div className="space-y-4">
      <StatusLine sticky={false} state={inSync ? 'ok' : 'warn'}>
        {inSync ? <>Production and staging are in sync.</> : <><span className="font-mono">{STAGING_DEP.tag}</span> is ready to promote to production.</>}
      </StatusLine>

      <div className="grid gap-px border bg-border lg:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)_auto_minmax(0,1fr)]">
        <EnvPane env={PROMO_ENVS[0]} dep={PREVIEW_DEP} />
        <div className="flex items-center justify-center gap-2 bg-background px-3 py-2 lg:flex-col">
          <ArrowRight className="h-3.5 w-3.5 text-muted-foreground lg:rotate-0" />
          <span className="text-center font-mono text-[10px] text-muted-foreground">auto on merge</span>
        </div>
        <EnvPane env={PROMO_ENVS[1]} dep={STAGING_DEP} />
        <div className="flex flex-col items-center justify-center gap-2 bg-background px-3 py-3">
          {inSync ? (
            <span className="inline-flex items-center gap-1 text-xs text-muted-foreground"><Check className="h-3.5 w-3.5" /> in sync</span>
          ) : (
            <EchoDialog
              echo={`$ temps deploy promote ${STAGING_DEP.tag} --to production`}
              title="Promote to production"
              description={`Reuses the image built for ${STAGING_DEP.tag} (${STAGING_DEP.commit} · ${STAGING_DEP.msg}). No rebuild. Production variables are rendered, health check /healthz runs, then routes switch. About 20 seconds, no downtime.`}
              confirmWord="production"
              steps={[`verify ${STAGING_DEP.tag} image present`, 'render production variables', 'start containers', 'health check /healthz', 'switch proxy routes', 'mark as current']}
              onDone={() => setProd({ ...STAGING_DEP, at: 'now' })}
              trigger={<Button size="sm" className="op-primary h-8 text-xs">promote <ArrowRight /> <Kbd keys="P" className="ml-1 opacity-70" /></Button>}
            />
          )}
          <span className="text-center font-mono text-[10px] text-muted-foreground">{inSync ? 'nothing to promote' : `${STAGING_DEP.commit} → production`}</span>
        </div>
        <EnvPane env={PROMO_ENVS[2]} dep={prod} />
      </div>

      <div className="op-rows border text-xs">
        <div className="op-row flex flex-wrap items-center gap-x-3 gap-y-1">
          <Status state="ok" label={PREV_PROD_DEP.tag} className="font-mono" />
          <span className="min-w-0 flex-1 truncate text-muted-foreground">{PREV_PROD_DEP.commit} · {PREV_PROD_DEP.msg} · {PREV_PROD_DEP.at}</span>
          <EchoDialog
            echo={`$ temps deploy rollback --to ${PREV_PROD_DEP.tag}`}
            title={`Roll back to ${PREV_PROD_DEP.tag}`}
            description={`Re-points production at the image already built for ${PREV_PROD_DEP.tag}. Roll back is the same operation as promote, pointed backwards, so it uses the same dialog.`}
            confirmWord={PREV_PROD_DEP.tag}
            steps={[`verify ${PREV_PROD_DEP.tag} image present`, 'render production variables', 'start containers', 'health check /healthz', 'switch proxy routes']}
            onDone={() => setProd({ ...PREV_PROD_DEP, at: 'now' })}
            trigger={<Button size="sm" variant="outline" className="h-6 px-2 text-[11px]"><RotateCcw /> roll back</Button>}
          />
        </div>
      </div>
    </div>
  )
}

// ── 6. Variables per environment, and bulk association ─────────────────

const VAR_ENVS = [{ id: 1, name: 'production', slug: 'production' }, { id: 2, name: 'staging', slug: 'staging' }] as const
type Var = { id: number; key: string; value: string; secret: boolean; envs: number[] }
const VARS0: Var[] = [
  { id: 1, key: 'DATABASE_URL', value: 'postgres://acme:••••@acme-pg:5432/acme', secret: true, envs: [1, 2] },
  { id: 2, key: 'SENTRY_DSN', value: 'https://temps.acme.sh/errors/ingest/7', secret: false, envs: [1, 2] },
  { id: 3, key: 'STRIPE_SECRET_KEY', value: 'sk_live_51H••••••••', secret: true, envs: [1] },
  { id: 4, key: 'STRIPE_TEST_KEY', value: 'sk_test_51H••••••••', secret: true, envs: [2] },
  { id: 5, key: 'FEATURE_RATE_LIMITS', value: 'true', secret: false, envs: [2] },
  { id: 6, key: 'RATE_LIMIT_PER_KEY', value: '600', secret: false, envs: [2] },
]
const envName = (id: number) => VAR_ENVS.find((e) => e.id === id)?.name ?? String(id)

function VariablesExample() {
  const [vars, setVars] = useState(VARS0)
  const [view, setView] = useState<'matrix' | number>(1)
  const [q, setQ] = useState('')
  const [sel, setSel] = useState<Set<number>>(new Set())
  const [reveal, setReveal] = useState<Set<number>>(new Set())

  const list = vars
    .filter((v) => (view === 'matrix' || v.envs.includes(view)) && v.key.toLowerCase().includes(q.toLowerCase()))
    .sort((a, b) => a.key.localeCompare(b.key))
  const missingInProd = vars.filter((v) => v.envs.includes(2) && !v.envs.includes(1) && !v.key.includes('TEST'))
  const selKeys = Array.from(sel).map((id) => vars.find((v) => v.id === id)?.key ?? '')
  const cols = view === 'matrix' ? '24px 1.6fr 1.6fr 80px 80px' : '24px 1.6fr 2fr 110px'

  const toggleCell = (id: number, envId: number, on: boolean) =>
    setVars((prev) => prev.map((v) => (v.id === id ? { ...v, envs: on ? v.envs.filter((e) => e !== envId) : [...v.envs, envId] } : v)))
  const bulk = (envId: number, add: boolean) => {
    setVars((prev) => prev.map((v) => (sel.has(v.id) ? { ...v, envs: add ? Array.from(new Set([...v.envs, envId])) : v.envs.filter((e) => e !== envId) } : v)))
    setSel(new Set())
  }

  return (
    <div className="space-y-4">
      <StatusLine sticky={false} state={missingInProd.length ? 'warn' : 'ok'}>
        {missingInProd.length ? (
          <><Phrase onClick={() => { setView('matrix'); setSel(new Set(missingInProd.map((v) => v.id))) }}>{missingInProd.length} variables</Phrase> exist in staging but not production.</>
        ) : (
          <>Production and staging receive the same variables.</>
        )}
      </StatusLine>

      <div className="flex flex-wrap items-center gap-2">
        <div role="tablist" className="op-scroll-x flex max-w-full border text-xs">
          {[...VAR_ENVS.map((e) => [e.id as 'matrix' | number, e.name] as const), ['matrix', 'matrix'] as const].map(([v, l], i) => (
            <button
              key={String(v)}
              role="tab"
              aria-selected={view === v}
              onClick={() => setView(v)}
              className={cn('inline-flex h-8 shrink-0 items-center whitespace-nowrap px-3', i > 0 && 'border-l', view === v ? 'bg-foreground text-background' : 'hover:bg-muted')}
            >
              {l}
            </button>
          ))}
        </div>
        <div className="relative min-w-0 flex-1 basis-40 sm:max-w-56">
          <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder="search keys" aria-label="Search keys" className="h-8 pl-7 pr-8 text-xs" />
          <Kbd keys="/" className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 opacity-60" />
        </div>
      </div>

      {typeof view === 'number' && (
        <p className="text-[11px] text-muted-foreground">
          Showing exactly what <span className="font-medium text-foreground">{envName(view)}</span> receives. Switch to{' '}
          <button type="button" className="underline underline-offset-4" onClick={() => setView('matrix')}>matrix</button> to compare or to change associations in bulk.
        </p>
      )}

      {list.length === 0 ? (
        <PageState state="empty" title={`No key matches "${q}"`} reason="Search matches the key only. Values are never searched." next={<Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => setQ('')}>clear search</Button>} />
      ) : (
        <div className="op-rows border" role="listbox" aria-multiselectable="true">
          <div className="op-row op-cols hidden items-center gap-x-3 md:grid" style={{ '--cols': cols } as CSSProperties}>
            <span />
            <span className="op-label">key</span>
            <span className="op-label">value</span>
            {view === 'matrix' ? VAR_ENVS.map((e) => <span key={e.id} className="op-label">{e.name}</span>) : <span className="op-label">also in</span>}
          </div>
          {list.map((v) => (
            <div
              key={v.id}
              role="option"
              aria-selected={sel.has(v.id)}
              className={cn('op-row op-cols grid grid-cols-[24px_minmax(0,1fr)] items-center gap-x-3 text-xs', sel.has(v.id) && 'op-marker-hot')}
              style={{ '--cols': cols } as CSSProperties}
            >
              <button
                type="button"
                aria-label={sel.has(v.id) ? `deselect ${v.key}` : `select ${v.key}`}
                onClick={() => setSel((s) => { const n = new Set(s); if (n.has(v.id)) n.delete(v.id); else n.add(v.id); return n })}
                className={cn('flex h-4 w-4 items-center justify-center border', sel.has(v.id) && 'bg-foreground text-background')}
              >
                {sel.has(v.id) && <Check className="h-3 w-3" />}
              </button>
              <span className="min-w-0 font-mono font-medium">
                <span className="block truncate">{v.key}{v.secret && <span className="ml-2 border px-1 text-[10px] font-normal text-muted-foreground">secret</span>}</span>
                <span className="mt-0.5 flex flex-wrap gap-1 font-sans text-[11px] font-normal text-muted-foreground md:hidden">
                  {view === 'matrix'
                    ? VAR_ENVS.map((e) => {
                        const on = v.envs.includes(e.id)
                        return (
                          <button key={e.id} type="button" aria-pressed={on} onClick={() => toggleCell(v.id, e.id, on)} className={cn('border px-1.5 font-mono', on && 'bg-foreground text-background')}>
                            {on ? '✓ ' : '– '}{e.name}
                          </button>
                        )
                      })
                    : v.envs.filter((id) => id !== view).map(envName).join(', ') || <span className="text-warning">only here</span>}
                </span>
              </span>
              <SecretValue className="hidden md:flex" value={v.value} secret={v.secret} revealed={reveal.has(v.id)} onToggle={() => setReveal((r) => { const n = new Set(r); if (n.has(v.id)) n.delete(v.id); else n.add(v.id); return n })} />
              {view === 'matrix'
                ? VAR_ENVS.map((e) => {
                    const on = v.envs.includes(e.id)
                    return (
                      <button
                        key={e.id}
                        type="button"
                        aria-pressed={on}
                        title={on ? `remove from ${e.name}` : `add to ${e.name}`}
                        onClick={() => toggleCell(v.id, e.id, on)}
                        className={cn('hidden h-6 w-14 items-center justify-center border font-mono text-[11px] md:inline-flex', on ? 'bg-foreground text-background' : 'text-muted-foreground hover:bg-muted')}
                      >
                        {on ? '✓' : '–'}
                      </button>
                    )
                  })
                : (
                  <span className="hidden truncate text-muted-foreground md:block">
                    {v.envs.filter((id) => id !== view).map(envName).join(', ') || <span className="text-warning">only here</span>}
                  </span>
                )}
            </div>
          ))}
          <div className="op-row flex flex-wrap items-center gap-x-1 gap-y-1 text-[11px] text-muted-foreground">
            {list.length} of {vars.length} · <Kbd keys="x" className="mx-1" /> select · <Kbd keys={['⇧', 'A']} className="mx-1" /> all · <Kbd keys="/" className="mx-1" /> search
          </div>
        </div>
      )}

      {sel.size > 0 && (
        <div className="op-sticky-bottom flex flex-wrap items-center gap-2 border bg-background px-3 py-2 text-xs">
          <span className="font-medium">{sel.size} selected</span>
          <span className="ml-1 text-muted-foreground">attach to</span>
          {VAR_ENVS.map((e) => (
            <EchoDialog
              key={`add-${e.id}`}
              echo={`$ temps env attach ${e.slug} ${selKeys.join(' ')}`}
              title={`Add to ${e.name}`}
              description={`${sel.size} variable${sel.size > 1 ? 's' : ''} will also be rendered into ${e.name} on its next deploy.`}
              confirmWord={e.slug}
              steps={['update associations', `render ${e.name} variables`, 'mark environment for redeploy']}
              onDone={() => bulk(e.id, true)}
              trigger={<Button size="sm" variant="outline" className="h-7 text-xs">{e.name}</Button>}
            />
          ))}
          <span className="ml-1 text-muted-foreground">detach from</span>
          {VAR_ENVS.map((e) => (
            <EchoDialog
              key={`rm-${e.id}`}
              echo={`$ temps env detach ${e.slug} ${selKeys.join(' ')}`}
              title={`Remove from ${e.name}`}
              description={`${e.name} stops receiving ${sel.size} variable${sel.size > 1 ? 's' : ''} on its next deploy. The variables keep existing for other environments.`}
              confirmWord={e.slug}
              steps={['update associations', `render ${e.name} variables`, 'mark environment for redeploy']}
              onDone={() => bulk(e.id, false)}
              trigger={<Button size="sm" variant="outline" className="h-7 text-xs">{e.name}</Button>}
            />
          ))}
          <button type="button" className="ml-auto text-muted-foreground underline underline-offset-4" onClick={() => setSel(new Set())}>clear <Kbd keys="esc" className="ml-1" /></button>
        </div>
      )}
    </div>
  )
}

// ── 7. Time and retention ──────────────────────────────────────────────

type PlanId = 'selfhost' | 'starter' | 'team' | 'business'
const PLANS: Record<PlanId, { label: string; retention: string; retentionDays: number; ingest: string | null; ingestGb: number | null; pitr: string }> = {
  selfhost: { label: 'self-hosted', retention: 'as configured · 90d', retentionDays: 90, ingest: null, ingestGb: null, pitr: 'as configured' },
  starter: { label: 'Cloud Starter', retention: '30d', retentionDays: 30, ingest: '10 GB/mo', ingestGb: 10, pitr: '7d' },
  team: { label: 'Cloud Team', retention: '90d', retentionDays: 90, ingest: '100 GB/mo', ingestGb: 100, pitr: '30d' },
  business: { label: 'Cloud Business', retention: '13 months', retentionDays: 395, ingest: '1 TB/mo', ingestGb: 1000, pitr: '90d' },
}
const INGEST_USED_GB = 11.4
const RANGES: readonly Range[] = [
  { label: '24h', days: 1 },
  { label: '7d', days: 7 },
  { label: '30d', days: 30 },
  { label: '90d', days: 90 },
  { label: '13mo', days: 395 },
]

function TimeExample() {
  const [planId, setPlanId] = useState<PlanId>('starter')
  const [range, setRange] = useState('24h')
  const [gated, setGated] = useState<string | null>(null)
  const plan = PLANS[planId]
  const sampled = plan.ingestGb !== null && INGEST_USED_GB > plan.ingestGb

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <label className="op-label" htmlFor="plan">plan</label>
        <select
          id="plan"
          value={planId}
          onChange={(e) => { setPlanId(e.target.value as PlanId); setGated(null) }}
          className="h-8 border bg-background px-2 font-mono text-xs"
        >
          {(Object.keys(PLANS) as PlanId[]).map((k) => <option key={k} value={k}>{PLANS[k].label}</option>)}
        </select>
        <RangePicker
          className="sm:ml-auto"
          ranges={RANGES}
          value={range}
          onChange={(l) => { setRange(l); setGated(null) }}
          retentionDays={plan.retentionDays}
          retentionLabel={plan.retention}
          onGated={(r) => setGated(planId === 'selfhost'
            ? `${r.label} is beyond the configured retention (${plan.retention}). Raise it in observability settings.`
            : `${r.label} is beyond ${plan.label} retention (${plan.retention}). Team keeps 90d, Business 13 months.`)}
        />
      </div>
      {gated && <p className="border border-destructive px-3 py-2 text-[11px]">{gated}</p>}
      <TimeChart
        data={CHART}
        series={[{ key: 'req', name: 'requests' }]}
        markers={CHART_MARKERS}
        unit="req"
        height={112}
        yTicks={[0, 1000, 2000]}
        xInterval={5}
        sampled={sampled ? { from: '14:00', to: '23:00', label: 'sampled 1 in 4' } : undefined}
      />
      <ChartFooter>
        <span>showing {range}</span>
        <span>· retention {plan.retention}</span>
        <span>· pitr {plan.pitr}</span>
        <span>· ┆ deploy</span>
        {sampled && <span>· ◌ sampled since 14:00 · {plan.ingest} allowance reached · {INGEST_USED_GB} GB used</span>}
      </ChartFooter>
      <p className="text-[11px] text-muted-foreground">
        {sampled
          ? 'Starter has used 11.4 GB of a 10 GB allowance, so telemetry is head-sampled — never silently dropped. The band, the footer and the status line all say so.'
          : `${plan.label} keeps ${plan.retention} and ingests ${plan.ingest ?? 'as much as the box holds'}. Nothing is sampled.`}
      </p>
    </div>
  )
}

// ── 8. Keyboard ────────────────────────────────────────────────────────

const KEYS: { keys: string | string[]; where: string; does: string; control: string }[] = [
  { keys: ['⌘', 'K'], where: 'everywhere', does: 'command palette', control: 'the search field in the header' },
  { keys: '/', where: 'ledger', does: 'focus the filter', control: 'the filter input itself' },
  { keys: 'j', where: 'ledger', does: 'move down', control: 'hover and click a row' },
  { keys: 'k', where: 'ledger', does: 'move up', control: 'hover and click a row' },
  { keys: '⏎', where: 'ledger', does: 'open the row', control: 'the row is a click target' },
  { keys: ['1', '6'], where: 'detail', does: 'switch to tab 1…6', control: 'the tab strip' },
  { keys: ['⌘', '⏎'], where: 'detail', does: 'the primary action (deploy)', control: 'the primary button' },
  { keys: ['⌘', 'S'], where: 'settings', does: 'click the save button', control: 'the sticky save bar' },
  { keys: 'd', where: 'everywhere', does: 'toggle density', control: 'the density control in the header' },
  { keys: 'esc', where: 'everywhere', does: 'close dialog, menu, selection', control: 'cancel in every dialog' },
]

function KeyboardExample() {
  return (
    <div className="op-rows border text-xs">
      <div className="op-row op-cols hidden items-center gap-x-3 md:grid" style={{ '--cols': '90px 110px 1.2fr 1.4fr' } as CSSProperties}>
        {['key', 'where', 'does', 'visible control'].map((h) => <span key={h} className="op-label">{h}</span>)}
      </div>
      {KEYS.map((k) => (
        <div key={`${k.where}-${k.does}`} className="op-row op-cols grid grid-cols-[1fr] items-center gap-x-3" style={{ '--cols': '90px 110px 1.2fr 1.4fr' } as CSSProperties}>
          <span className="flex min-w-0 flex-wrap items-center gap-2">
            <Kbd keys={k.keys} />
            <span className="text-muted-foreground md:hidden">{k.does} · {k.control}</span>
          </span>
          <span className="hidden text-muted-foreground md:block">{k.where}</span>
          <span className="hidden truncate md:block">{k.does}</span>
          <span className="hidden truncate text-muted-foreground md:block">{k.control}</span>
        </div>
      ))}
    </div>
  )
}

// ── 9. Responsive ──────────────────────────────────────────────────────

const RESP_ROW = {
  name: 'events-ch',
  engine: 'ClickHouse 24',
  size: '38 GB',
  backup: '3d ago',
  note: 'backup older than 24h',
}

function ResponsiveExample() {
  const [seg, setSeg] = useState<'overview' | 'deploys' | 'environments' | 'variables' | 'logs' | 'settings'>('overview')
  return (
    <div className="space-y-5">
      <div>
        <p className="op-label mb-2">desktop · cells in the grid</p>
        <div className="op-rows border text-xs">
          <div className="op-row op-cols hidden items-center gap-x-3 md:grid" style={{ '--cols': DB_GRID } as CSSProperties}>
            {['name', 'state', 'size', 'last backup', 'pitr'].map((h) => <span key={h} className="op-label">{h}</span>)}
          </div>
          <div className="op-row op-cols grid grid-cols-[1fr_auto] items-center gap-x-3 text-xs" style={{ '--cols': DB_GRID } as CSSProperties}>
            <span className="min-w-0 truncate font-medium md:hidden">{RESP_ROW.name}</span>
            <span className="md:hidden"><Status state="warn" label="" /></span>
            <span className="hidden min-w-0 truncate font-medium md:block">{RESP_ROW.name}</span>
            <span className="hidden min-w-0 truncate md:block"><Status state="warn" label={RESP_ROW.note} /></span>
            <span className="hidden md:block"><Num value={RESP_ROW.size} /></span>
            <span className="hidden text-muted-foreground md:block">{RESP_ROW.backup}</span>
            <span className="hidden md:block"><Num value={null} /></span>
          </div>
        </div>
      </div>

      <div>
        <p className="op-label mb-2">phone · the `mobile` node, in a 320px frame</p>
        <div className="w-[320px] max-w-full border">
          <div className="op-rows text-xs">
            <div className="op-row grid grid-cols-[1fr_auto] items-center gap-x-3">
              <span className="min-w-0">
                <span className="block truncate font-medium">{RESP_ROW.name}</span>
                <span className="block truncate text-[11px] text-muted-foreground">{RESP_ROW.note} · {RESP_ROW.size}</span>
              </span>
              <Status state="warn" label="" />
            </div>
          </div>
        </div>
        <p className="mt-2 text-[11px] text-muted-foreground">
          Name, the note that explains the state, and the glyph. Nothing the desktop row can reach is unreachable here: if the row had a promote or roll back action, the mobile node carries it too.
        </p>
      </div>

      <div>
        <p className="op-label mb-2">tab strips scroll · .op-scroll-x in a 320px frame</p>
        <div className="w-[320px] max-w-full">
          <Segmented
            value={seg}
            onChange={setSeg}
            options={[
              ['overview', 'overview'],
              ['deploys', 'deploys'],
              ['environments', 'environments'],
              ['variables', 'variables'],
              ['logs', 'logs'],
              ['settings', 'settings'],
            ] as const}
          />
        </div>
      </div>

      <div>
        <p className="op-label mb-2">action groups wrap · w-full sm:w-auto sm:ml-auto</p>
        <div className="w-[320px] max-w-full border p-2">
          <div className="flex w-full flex-wrap gap-2 sm:ml-auto sm:w-auto">
            <Button size="sm" variant="outline" className="h-8 text-xs">import .env</Button>
            <Button size="sm" variant="outline" className="h-8 text-xs">show values</Button>
            <Button size="sm" className="op-primary h-8 text-xs"><Plus /> add variable</Button>
          </div>
        </div>
      </div>
    </div>
  )
}

// ── Page ───────────────────────────────────────────────────────────────

function StateExamples() {
  const [retrying, setRetrying] = useState(false)
  return (
    <>
      <Demo label="loading · skeleton rows, never a spinner">
        <PageState state="loading" rows={3} />
      </Demo>
      <Demo label="empty · nothing yet, and the next step">
        <PageState
          state="empty"
          title="No databases yet"
          reason="A database runs as a container on this host and is reachable from every service in the project by its name."
          next={<Button size="sm" className="op-primary h-8 text-xs"><Plus /> new database</Button>}
        />
      </Demo>
      <Demo label="unconfigured · names what is missing, shows an example, links to the fix">
        <PageState
          state="unconfigured"
          title="Session replay is not receiving anything"
          missing="the @temps-sdk/web snippet on acme-storefront, and a blob store for the recordings"
          settingsHref="/settings"
          settingsLabel="open replay settings"
          example={
            <div className="op-rows text-xs">
              <div className="flex items-center gap-3 py-1"><span className="font-mono">sess_9f31c</span><span className="text-muted-foreground">/checkout → submit → error</span><span className="ml-auto font-mono">1:24</span></div>
              <div className="flex items-center gap-3 py-1"><span className="font-mono">sess_7c02a</span><span className="text-muted-foreground">/pricing → /signup</span><span className="ml-auto font-mono">0:38</span></div>
            </div>
          }
        />
      </Demo>
      <Demo label="error · the message, the resource, a retry">
        <PageState
          state="error"
          title="Could not load databases"
          message="connection refused (os error 111) querying acme-pg:5432"
          resource="project acme · host fsn1"
          retrying={retrying}
          onRetry={() => { setRetrying(true); window.setTimeout(() => setRetrying(false), 1200) }}
        />
      </Demo>
    </>
  )
}

export function PatternsPage() {
  return (
    <DocPage
      eyebrow="page patterns · every screen is one of three"
      intro={
        <>
          A new console screen is a <span className="font-mono">Ledger</span>, a <span className="font-mono">Detail</span> or a{' '}
          <span className="font-mono">Settings</span>. A screen that does not fit is a reason to extend a template, not to start from a blank
          div. Below each template are the four cross-cutting patterns every screen has to get right: the non-happy states, promote and roll
          back, per-environment variables, time and retention, the keyboard, and the phone fold. Every example is live — click, filter, promote,
          select, change the plan.
        </>
      }
      toc={TOC}
    >
      <Block
        id="ledger"
        title="Ledger · the list screen"
        api={`<Ledger
  title="Databases" meta="5 managed · acme · fsn1"
  status={<StatusLine state={worst(states)}>…</StatusLine>}
  columns={[{ label: 'name', key: 'name' }, 'state', { label: 'size', key: 'size', numeric: true }, …]}
  // rows carry sort: { name, size, backup, pitr } · header click cycles asc → desc → off
  grid="1.3fr 1.2fr 90px 110px 90px"
  rows={rows} total={5} dense={false}
  filter={q} onFilter={setQ} placeholder="filter databases"
  action={<Button className="op-primary">new database</Button>}
  footer={<>3 of 5 · 1 needs attention · …</>}
/>`}
        rule={
          <>
            <p>Title and one line of mono facts, then the verdict, then a filter on <Kbd keys="/" />, then one action. Rows sort attention-first through <code>STATE_RANK</code>, so what is broken is the first thing under the header, always.</p>
            <Rule state="ok">The status line names the row that is wrong and links to it. It is a verdict, not an inventory.</Rule>
            <Rule state="error">A count in the status line ("5 databases, 1 warning"). Counts belong in the footer.</Rule>
            <p>The footer carries the counts and the keys. Projects, databases, deploys, domains, backups, sandboxes and errors are all this screen.</p>
          </>
        }
      >
        <Demo label="databases · one row in warn, and the line says which">
          <LedgerExample />
        </Demo>
      </Block>

      <Block
        id="detail"
        title="Detail · the thing screen"
        api={`<Detail
  title="api-gateway" meta="production · dep_91a · main"
  status={<StatusLine state="warn">…</StatusLine>}
  tabs={['overview','deploys','settings']} tab={tab} onTab={setTab}
  actions={<><Segmented … /><Button className="op-primary">deploy ⌘⏎</Button></>}
>
  <TimeChart markers={deploys} hot={hot} onHot={setHot} />
  <MetricGrid cols={4}><Metric baseline="since dep_91a" … /></MetricGrid>
  <div className="op-raise">…the incident thread…</div>
</Detail>`}
        rule={
          <>
            <p>Identity in the title, verdict in the line, tabs with number keys, actions on the right. The body is always the same three things in the same order: one <code>TimeChart</code> with deploy markers, one <code>MetricGrid</code> whose every tile names a baseline, one <code>.op-raise</code> — the thread the reader is meant to act on.</p>
            <Rule state="ok">Hovering a deploy row lights its marker on the chart, and the reverse. The deploy and the metric are the same object.</Rule>
            <Rule state="error">More than one raised element. If two things are raised, neither is.</Rule>
            <p>Press <Kbd keys="1" /> <Kbd keys="2" /> <Kbd keys="3" /> to switch tabs; roll back lives in the deploys tab.</p>
          </>
        }
      >
        <Demo label="project overview · chart, metrics, one raised thread">
          <DetailExample />
        </Demo>
      </Block>

      <Block
        id="settings"
        title="Settings · the form screen"
        api={`<Settings
  title="api-gateway settings" meta="production · acme"
  status={<StatusLine state={dirty ? 'warn' : 'ok'}>…</StatusLine>}
  sections={[{ title: 'source', body: <Field label="branch"><Picker … /></Field> }]}
  dirty={dirty} onSave={save}
  danger={<EchoDialog destructive … />}
/>`}
        rule={
          <>
            <p>Sections with a side index, <code>Field</code> rows (label, control, help) that go to one line via a container query so they stack when the section is narrow regardless of viewport, and a sticky save bar that <Kbd keys={['⌘', 'S']} /> literally clicks — so pressed and disabled states stay honest.</p>
            <Rule state="ok">A branch is a <code>Picker</code>: the operator recognises branches, they do not recall them.</Rule>
            <Rule state="error">A plain <code>&lt;select&gt;</code> for branches, images, regions or environments.</Rule>
            <p>The danger zone's only action is an <code>EchoDialog</code>: the CLI command, a typed confirmation, and the backend's own steps.</p>
          </>
        }
      >
        <Demo label="two sections, a picker, a switch, a checkbox, ⌘S and a danger zone">
          <SettingsExample />
        </Demo>
      </Block>

      <Block
        id="states"
        title="The four non-happy states"
        api={`<PageState state="loading" rows={3} />
<PageState state="empty" title reason next />
<PageState state="unconfigured" title missing example settingsHref settingsLabel />
<PageState state="error" title message resource onRetry retrying />`}
        rule={
          <>
            <p>Every surface that can be not-happy goes through one component. Nothing renders blank, and a spinner is never a page state.</p>
            <Rule state="ok">Unconfigured shows the surface anyway: what is missing, an example of what it will show, a link straight to the settings that fix it.</Rule>
            <Rule state="error">Hiding a feature because it is not configured. The user then concludes Temps cannot do it.</Rule>
            <p>The self-hosted operator has no support channel. A failure that needs a restart to notice is a design failure.</p>
          </>
        }
      >
        <StateExamples />
      </Block>

      <Block
        id="promote"
        title="Promote and roll back"
        api={`<EchoDialog
  echo="$ temps deploy promote dep_93c --to production"
  title="Promote to production" confirmWord="production"
  steps={['verify image', 'render variables', 'health check /healthz', …]}
  trigger={<Button className="op-primary">promote →</Button>}
/>`}
        rule={
          <>
            <p>Promotion is the shape of the product: the same image moves preview → staging → production, and only the variables change. So the console draws the path and puts the action on it.</p>
            <Rule state="ok">Promote is a main action on the environments surface, and a visible button on every deploy that is ahead of production.</Rule>
            <Rule state="error">Promote hidden in a per-row ⋯ menu. That is where it used to live, and nobody found it.</Rule>
            <p>Roll back is the same operation pointed backwards, so it is the same dialog, reached from the detail's deploys tab.</p>
          </>
        }
      >
        <Demo label="the promotion path · promote opens the echo dialog, and the panes update">
          <PromoteExample />
        </Demo>
      </Block>

      <Block
        id="variables"
        title="Variables per environment, and bulk association"
        api={`view: production | staging | matrix
$ temps env attach staging FEATURE_RATE_LIMITS RATE_LIMIT_PER_KEY
$ temps env detach production STRIPE_TEST_KEY`}
        rule={
          <>
            <p>Each environment is its own view showing exactly what that environment receives, with an "also in" / "only here" column. The matrix is the single cross-environment view: one column per environment, each cell a toggle.</p>
            <Rule state="ok">Select rows, then a sticky bulk bar offers attach / detach per environment, each through an <code>EchoDialog</code>.</Rule>
            <Rule state="error">One global list with a pill per environment and a header dropdown that only changes preview values. Choosing staging still showed production-only variables.</Rule>
            <p>Search matches the key, never the value. The status line's "2 variables exist in staging but not production" selects those two and opens the matrix.</p>
          </>
        }
      >
        <Demo label="6 variables · switch view, search, select, attach in bulk">
          <VariablesExample />
        </Demo>
      </Block>

      <Block
        id="time"
        title="Time and retention"
        api={`<RangePicker ranges={RANGES} value={range} onChange={setRange}
  retentionDays={plan.retentionDays} retentionLabel={plan.retention}
  onGated={(r) => say(\`\${r.label} is beyond \${plan.label} retention\`)} />
<ChartFooter>showing 24h · retention 30d · ┆ deploy · ◌ sampled …</ChartFooter>`}
        rule={
          <>
            <p>Retention is a plan property, so the plan is a design input, not a billing detail. Change the plan below and the gate, the footer and the sampled band all move.</p>
            <Rule state="ok">Ranges past retention are struck through and still clickable — clicking says which plan keeps that range.</Rule>
            <Rule state="error">Hiding a range the plan does not cover. The reader then cannot tell "not available" from "not there".</Rule>
            <p>Pricing promises that telemetry past the allowance is head-sampled "and the console says so; it is never silently dropped". That is a UI contract: the band, the footer and the status line all carry it.</p>
          </>
        }
      >
        <Demo label="plan → retention gate → sampled band, live">
          <TimeExample />
        </Demo>
      </Block>

      <Block
        id="keyboard"
        title="The keyboard model"
        api={`// Keys are ignored while an input has focus.
// Every key has a visible badge AND a visible control.`}
        rule={
          <>
            <p>Ten keys, the same everywhere. They are accelerators for people who live in the console; they are never the only way to reach anything.</p>
            <Rule state="ok">Every shortcut has a visible control that does the same thing, and a <code>Kbd</code> badge on that control.</Rule>
            <Rule state="error">A feature reachable only by a chord. A keyboard-only feature does not exist for the person who has not read the docs.</Rule>
            <p>This page is itself a demonstration: <Kbd keys="j" /> <Kbd keys="k" /> move the ledger above, <Kbd keys="1" />–<Kbd keys="3" /> switch the detail's tabs, <Kbd keys={['⌘', 'S']} /> saves the settings form.</p>
          </>
        }
      >
        <Demo label="the whole model, on one screen">
          <KeyboardExample />
        </Demo>
      </Block>

      <Block
        id="responsive"
        title="Responsive rules"
        api={`.op-cols   grid-template-columns: var(--cols) from md up
.op-scroll-x  tab strips and segmented controls scroll, never wrap
@media (max-width: 767px) { .op-row { height: auto } }`}
        rule={
          <>
            <p>Verified at 390 and 1440 with a scrollWidth check on every v5 screen. Four rules cover it.</p>
            <Rule state="ok">A ledger row hides its cells below md and renders <code>mobile</code>: name, the note that explains the state, the glyph — and the row's primary action.</Rule>
            <Rule state="error">A desktop-only cell that carries the only promote or roll back button. A phone user cannot reach it.</Rule>
            <p>Tab strips scroll with <code>.op-scroll-x</code>. Action groups wrap and take full width below sm (<code>w-full sm:w-auto sm:ml-auto</code>), never <code>ml-auto</code> alone on three buttons. Rows are fixed height on desktop and grow with content on phones.</p>
          </>
        }
      >
        <Demo label="one row, both folds">
          <ResponsiveExample />
        </Demo>
      </Block>
    </DocPage>
  )
}

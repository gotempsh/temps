// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState, type CSSProperties } from 'react'
import { ExternalLink, Moon, Plus, Square, Terminal as TerminalIcon, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { LogViewer, type LogLine } from '@/components/ui/log-viewer'
import {
  Callout, ChartFooter, Columns, Detail, EchoDialog, Histogram, KeyValue, Ledger, Lede, Metric, MetricGrid, Num, PageState, PageTitle, Phrase, RangePicker, Section, Segmented, Status, StatusLine, TimeChart, Waterfall, type Pct,
  type LedgerRow, type Range, type State, type TimeRange, type Span as VizSpan,
} from '@/components/op'
import { matches } from './ConsoleV5Admin'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   Observe + sandboxes on v5, using the real API shapes
   (SandboxInner, SandboxEvent, SandboxStatusResponse, TraceSummary,
   SpanRecord, SpanStats, MetricBucket in web/src/api/client/types.gen.ts).

   Real pages these replace: pages/Sandboxes.tsx (752 lines),
   pages/SandboxDetail.tsx (1,365), pages/TracesList.tsx (1,245),
   pages/TraceDetail.tsx (883), pages/MetricsExplorer.tsx (1,501).
   ──────────────────────────────────────────────────────────────────────── */

export type Notify = (level: 'ok' | 'warn' | 'err', msg: string, detail?: string) => void
export type Plan = { id: string; label: string; retention: string; retentionDays: number; sampled: boolean; ingest: string | null }
const RANGES: readonly Range[] = [{ label: '1h', days: 0.05 }, { label: '24h', days: 1 }, { label: '7d', days: 7 }, { label: '30d', days: 30 }, { label: '90d', days: 90 }]

function gated(notify: Notify, plan: Plan) {
  return (r: Range) => notify('warn', `${r.label} is beyond this plan's retention`, plan.id === 'selfhost' ? `raise retention in settings · currently ${plan.retention}` : `${plan.label} keeps ${plan.retention}`)
}

// ── Sandboxes ──────────────────────────────────────────────────────────

type Sandbox = { id: string; name: string; status: 'running' | 'sleeping' | 'stopped' | 'failed' | 'starting'; lifecycle: 'persistent' | 'ephemeral'; runtime: string; image: string; vcpus: number; memory: number; disk_size_mb: number; region: string; backend: 'docker' | 'firecracker'; cwd: string; source_repo_url: string | null; agent_run_id: number | null; preview_url_template: string; preview_password_hint: string | null; createdAt: string; timeout: number; cpu_pct: number; mem_pct: number; disk_pct: number }
const SANDBOXES: Sandbox[] = [
  { id: 'sbx_7f21', name: 'checkout-fix', status: 'running', lifecycle: 'persistent', runtime: 'bun 1.2', image: 'temps/sandbox:node22', vcpus: 2, memory: 4096, disk_size_mb: 10240, region: 'local', backend: 'docker', cwd: '/workspace/acme-web', source_repo_url: 'github.com/acme/acme-web', agent_run_id: 418, preview_url_template: 'https://sbx-7f21-3000.preview.acme.sh', preview_password_hint: 'ch••••', createdAt: '2h ago', timeout: 3600, cpu_pct: 34, mem_pct: 61, disk_pct: 22 },
  { id: 'sbx_a903', name: 'rate-limits-spike', status: 'running', lifecycle: 'ephemeral', runtime: 'node 22', image: 'temps/sandbox:node22', vcpus: 1, memory: 2048, disk_size_mb: 4096, region: 'local', backend: 'docker', cwd: '/workspace/api-gateway', source_repo_url: 'github.com/acme/api-gateway', agent_run_id: 421, preview_url_template: 'https://sbx-a903-8080.preview.acme.sh', preview_password_hint: null, createdAt: '25m ago', timeout: 1800, cpu_pct: 88, mem_pct: 40, disk_pct: 9 },
  { id: 'sbx_c1d4', name: 'docs-refresh', status: 'sleeping', lifecycle: 'persistent', runtime: 'python 3.12', image: 'temps/sandbox:python312', vcpus: 1, memory: 2048, disk_size_mb: 4096, region: 'local', backend: 'docker', cwd: '/workspace/docs', source_repo_url: 'github.com/acme/docs', agent_run_id: null, preview_url_template: 'https://sbx-c1d4-8000.preview.acme.sh', preview_password_hint: null, createdAt: '3d ago', timeout: 3600, cpu_pct: 0, mem_pct: 0, disk_pct: 31 },
  { id: 'sbx_e77b', name: 'billing-repro', status: 'failed', lifecycle: 'ephemeral', runtime: 'node 22', image: 'temps/sandbox:node22-playwright', vcpus: 2, memory: 4096, disk_size_mb: 8192, region: 'local', backend: 'docker', cwd: '/workspace/billing-worker', source_repo_url: 'github.com/acme/billing-worker', agent_run_id: 419, preview_url_template: '', preview_password_hint: null, createdAt: '41m ago', timeout: 1800, cpu_pct: 0, mem_pct: 0, disk_pct: 0 },
]
const SBX_STATE: Record<Sandbox['status'], State> = { running: 'ok', starting: 'warn', sleeping: 'idle', stopped: 'idle', failed: 'error' }
const SBX_STATUS = { docker_available: true, firecracker_available: false, image_name: 'temps/sandbox:node22', image_ready: true }

const SBX_EVENTS = [
  { at: '14:02:11', event_type: 'created', detail: 'persistent · 2 vCPU · 4 GB · 10 GB disk' },
  { at: '14:02:14', event_type: 'image_pulled', detail: 'temps/sandbox:node22 · cached' },
  { at: '14:02:19', event_type: 'repo_cloned', detail: 'github.com/acme/acme-web@develop · 1,204 files' },
  { at: '14:02:31', event_type: 'ready', detail: 'bun install · 12.3s' },
  { at: '14:03:02', event_type: 'preview_bound', detail: ':3000 → sbx-7f21-3000.preview.acme.sh · password set' },
  { at: '15:41:50', event_type: 'agent_run_started', detail: 'run #418 · "fix address form null id" · claude-sonnet' },
  { at: '15:58:07', event_type: 'agent_run_finished', detail: '3 files changed · tests green · branch fix/address-null pushed' },
]
const SBX_LOG: LogLine[] = [
  { ts: '15:57:41', level: 'info', msg: '$ bun test', fields: {} },
  { ts: '15:57:44', level: 'info', msg: '42 pass · 0 fail · 118 expect() calls', fields: { took: '2.9s' } },
  { ts: '15:57:45', level: 'info', msg: '$ git push -u origin fix/address-null', fields: {} },
  { ts: '15:57:49', level: 'info', msg: 'branch pushed', fields: { commit: 'd41f9e0' } },
  { ts: '15:58:07', level: 'info', msg: 'agent run finished', fields: { run: 418, files: 3 } },
]

export function SandboxesScreen({ go, dense }: { go: (v: string) => void; dense: boolean }) {
  const [q, setQ] = useState('')
  const list = SANDBOXES.filter((s) => matches(q, s.name, s.id, s.status, s.runtime, s.source_repo_url))
  const running = SANDBOXES.filter((s) => s.status === 'running').length
  const failed = SANDBOXES.find((s) => s.status === 'failed')
  const rows: LedgerRow[] = list.map((s) => ({
    id: s.id, state: SBX_STATE[s.status], onOpen: () => go(`sandbox:${s.id}`),
    mobile: <><span className="block font-medium">{s.name}</span><span className="block truncate text-[11px] text-muted-foreground">{s.status} · {s.runtime} · {s.createdAt}</span></>,
    cells: [
      <span className="font-medium">{s.name}<span className="ml-2 font-mono text-[11px] text-muted-foreground">{s.id}</span></span>,
      <Status state={SBX_STATE[s.status]} label={s.status === 'failed' ? 'image pull failed' : s.status} />,
      <span className="text-muted-foreground">{s.lifecycle}</span>,
      <span className="font-mono text-muted-foreground">{s.runtime}</span>,
      <span className="font-mono tabular-nums">{s.vcpus} vCPU · {s.memory / 1024} GB · {s.disk_size_mb / 1024} GB</span>,
      <span className="truncate font-mono text-muted-foreground">{s.source_repo_url ?? '–'}</span>,
      s.agent_run_id ? <span className="font-mono text-muted-foreground">run #{s.agent_run_id}</span> : <Num value={null} />,
      <span className="text-muted-foreground">{s.createdAt}</span>,
    ],
  }))
  return (
    <div className="space-y-6">
      <Ledger
        title="Sandboxes" meta={`${SANDBOXES.length} in this project · docker`}
        status={
          <StatusLine state={failed ? 'error' : 'ok'}>
            {failed ? <><Phrase onClick={() => go(`sandbox:${failed.id}`)}>{failed.name}</Phrase> failed to start.</> : <>{running} sandboxes running.</>}
          </StatusLine>
        }
        columns={['sandbox', 'status', 'lifecycle', 'runtime', 'resources', 'source', 'agent run', 'age']} grid="1.5fr 1.2fr 80px 90px 1.3fr 1.4fr 70px 60px"
        rows={rows} total={SANDBOXES.length} filter={q} onFilter={setQ} placeholder="filter sandboxes" hint="running first, then failed" dense={dense}
        action={<Button size="sm" className="op-primary h-8 text-xs"><Plus /> <span className="hidden sm:inline">new sandbox</span></Button>}
      />
      <div className="flex flex-wrap gap-x-6 gap-y-1 text-[11px] text-muted-foreground">
        <span><Status state={SBX_STATUS.docker_available ? 'ok' : 'error'} label="docker" /></span>
        <span><Status state={SBX_STATUS.firecracker_available ? 'ok' : 'idle'} label="firecracker · not available on this host" /></span>
        <span><Status state={SBX_STATUS.image_ready ? 'ok' : 'warn'} label={`${SBX_STATUS.image_name} ready`} /></span>
      </div>
    </div>
  )
}

const SBX_TABS = ['overview', 'events', 'logs'] as const
export function SandboxScreen({ id, notify, dense, go }: { id: string; notify: Notify; dense: boolean; go: (v: string) => void }) {
  const s0 = SANDBOXES.find((s) => s.id === id)
  const [tab, setTab] = useState<(typeof SBX_TABS)[number]>('overview')
  const [status, setStatus] = useState(s0?.status ?? 'stopped')
  const [lines, setLines] = useState<LogLine[]>([])
  useEffect(() => { let i = 0; const t = window.setInterval(() => setLines((p) => (i < SBX_LOG.length ? [...p, SBX_LOG[i++]] : p)), 400); return () => window.clearInterval(t) }, [])
  if (!s0) return <PageState state="empty" title="No such sandbox" reason={`${id} is not in this project.`} next={<Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => go('sandboxes')}>back to sandboxes</Button>} />
  const s = { ...s0, status }
  const st = SBX_STATE[s.status]

  if (s.status === 'failed') {
    return (
      <Detail title={s.name} meta={`${s.id} · ${s.image} · ${s.region}`} status={<StatusLine state="error">Image pull for <span className="font-mono">{s.image}</span> timed out.</StatusLine>} tabs={SBX_TABS} tab={tab} onTab={setTab}
        actions={<EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> destroy</Button>} echo={`$ temps sandbox destroy ${s.id}`} title="Destroy sandbox" description="Removes the container and its disk. Nothing was cloned, so nothing is lost." confirmWord={s.id} steps={['remove container', 'release disk', 'delete record']} onDone={() => { notify('warn', `${s.name} destroyed`); go('sandboxes') }} />}>
        <PageState state="error" title="Image pull timed out" message={`pull temps/sandbox:node22-playwright: context deadline exceeded (120s) · registry.temps.sh · 412 MB of 1.9 GB`} resource={`sandbox · ${s.id} · docker`} onRetry={() => { notify('ok', 'retrying image pull', s.image); setStatus('starting'); window.setTimeout(() => setStatus('running'), 1500) }} />
      </Detail>
    )
  }

  return (
    <Detail
      title={s.name} meta={`${s.id} · ${s.image} · ${s.region}`}
      status={
        <StatusLine state={st}>
          {s.status === 'running' && s.agent_run_id ? <><Phrase>Run #{s.agent_run_id}</Phrase> finished, tests green.</> : s.status === 'sleeping' ? <>Wakes on the next request to its preview URL; the disk is kept.</> : <>Nothing to do: {s.name} holds no work.</>}
        </StatusLine>
      }
      lede={
        <Lede state={st} word={s.status} facts={[
          { k: 'cpu', v: `${s.status === 'running' ? s.cpu_pct : 0}% of ${s.vcpus} vCPU`, state: s.status === 'running' && s.cpu_pct > 80 ? 'warn' : undefined },
          { k: 'memory', v: `${s.status === 'running' ? ((s.memory * s.mem_pct) / 100 / 1024).toFixed(1) : '0'} of ${s.memory / 1024} GB` },
          { k: 'disk', v: `${((s.disk_size_mb * s.disk_pct) / 100 / 1024).toFixed(1)} of ${s.disk_size_mb / 1024} GB` },
          { k: 'uptime', v: s.status === 'running' ? '2h 14m' : '–' },
          { k: 'lifecycle', v: s.lifecycle === 'ephemeral' ? `ephemeral · ${s.timeout / 60}m` : 'persistent' },
          { k: 'agent run', v: s.agent_run_id ? `#${s.agent_run_id} · finished` : 'none' },
        ]}>
          Cloned from {s.source_repo_url ?? 'no repository'}, created {s.createdAt}.
        </Lede>
      }
      tabs={SBX_TABS} tab={tab} onTab={setTab}
      actions={<>
        {s.preview_url_template && <Button size="sm" variant="outline" className="h-8 text-xs" asChild><a href={s.preview_url_template} target="_blank" rel="noreferrer"><ExternalLink /> preview{s.preview_password_hint && <span className="ml-1 font-mono text-[10px] text-muted-foreground">pw {s.preview_password_hint}</span>}</a></Button>}
        <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => notify('ok', 'terminal attached', `${s.id} · ${s.cwd}`)}><TerminalIcon /> terminal</Button>
        {s.agent_run_id && <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => notify('ok', 'pull request opened', `fix/address-null · run #${s.agent_run_id}`)}>open pull request</Button>}
        {s.agent_run_id && <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => notify('ok', 'deploy queued', `fix/address-null → staging`)}>deploy to staging</Button>}
        {s.status === 'running' ? (
          <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => { setStatus('sleeping'); notify('ok', `${s.name} sleeping`, 'disk kept · wakes on access') }}><Moon /> sleep</Button>
        ) : (
          <Button size="sm" className="op-primary h-8 text-xs" onClick={() => { setStatus('running'); notify('ok', `${s.name} woke`, '1.8s') }}>wake</Button>
        )}
        <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Square /> destroy</Button>} echo={`$ temps sandbox destroy ${s.id}`} title="Destroy sandbox" description={`Removes the container and its ${s.disk_size_mb / 1024} GB disk. The branch pushed by run #${s.agent_run_id ?? '–'} stays in the repository.`} confirmWord={s.id} steps={['stop container', 'unbind preview route', 'release disk', 'delete record']} onDone={() => { notify('warn', `${s.name} destroyed`); go('sandboxes') }} />
      </>}
    >
      {tab === 'overview' && (
        <Columns>
          <div>
            <Section title={`Agent run #${s.agent_run_id ?? '–'}`} meta="finished 16m ago · 3 files changed">
              <div className="border p-3 text-xs">
                <p className="font-medium">"fix address form null id"</p>
                <p className="mt-1 text-muted-foreground">claude-sonnet · 41 tool calls · 12,300 tokens · pushed <span className="font-mono">fix/address-null</span></p>
                <ul className="op-rows mt-2 min-w-0 border font-mono text-[11px]">
                  {['src/checkout/AddressForm.tsx  +14 −3', 'src/checkout/AddressForm.test.tsx  +22', 'src/lib/address.ts  +4 −1'].map((f) => <li key={f} className="truncate px-2 py-1">{f}</li>)}
                </ul>
              </div>
            </Section>
            <Section title="Terminal" meta={s.status === 'running' ? 'attached' : 'sandbox asleep'}>
              <div className="op-inset border font-mono text-[11px]">
                <pre className="max-h-40 overflow-auto p-3 leading-5">{`$ git status --short
 M src/checkout/AddressForm.tsx
 M src/checkout/AddressForm.test.tsx
 M src/lib/address.ts
$ bun test
 42 pass · 0 fail · 2.9s
$ █`}</pre>
              </div>
            </Section>
          </div>
          <div>
            <Section title="Runtime" meta="4">
              <KeyValue compact rows={[
                { k: 'runtime', v: s.runtime },
                { k: 'working directory', v: s.cwd },
                { k: 'preview', v: s.preview_url_template || '–' },
                { k: 'backend', v: `${s.backend} · ${s.region}` },
              ]} />
            </Section>
          </div>
        </Columns>
      )}
      {tab === 'events' && (
        <ol className="op-rows border text-xs">
          {SBX_EVENTS.map((e) => (
            <li key={e.at} className={cn('grid grid-cols-[72px_160px_1fr] items-baseline gap-3 px-3', dense ? 'py-1' : 'py-2')}>
              <span className="font-mono tabular-nums text-muted-foreground">{e.at}</span>
              <span className="font-mono">{e.event_type}</span>
              <span className="truncate text-muted-foreground">{e.detail}</span>
            </li>
          ))}
        </ol>
      )}
      {tab === 'logs' && <LogViewer lines={lines} title={`${s.id} · stdout`} className="max-h-96" />}
    </Detail>
  )
}

// ── Traces ─────────────────────────────────────────────────────────────

type Trace = { trace_id: string; root_span_name: string; service_name: string; deployment_environment: string; duration_ms: number; span_count: number; error_count: number; status_code: 'OK' | 'ERROR' | 'UNSET'; start_time: string; kind: 'SERVER' | 'CLIENT' | 'CONSUMER' }
const TRACES: Trace[] = [
  { trace_id: '3f9c1e7a8b2d4f60', root_span_name: 'POST /checkout', service_name: 'api-gateway', deployment_environment: 'production', duration_ms: 412, span_count: 12, error_count: 1, status_code: 'ERROR', start_time: '20:38:41', kind: 'SERVER' },
  { trace_id: '9a0d44c2e1f7b3a5', root_span_name: 'POST /checkout', service_name: 'api-gateway', deployment_environment: 'production', duration_ms: 388, span_count: 12, error_count: 1, status_code: 'ERROR', start_time: '20:37:12', kind: 'SERVER' },
  { trace_id: 'b71e0c9d5a3f2e84', root_span_name: 'GET /api/products', service_name: 'api-gateway', deployment_environment: 'production', duration_ms: 1840, span_count: 31, error_count: 0, status_code: 'OK', start_time: '20:36:58', kind: 'SERVER' },
  { trace_id: 'c2d3e4f5a6b7c8d9', root_span_name: 'GET /api/products', service_name: 'api-gateway', deployment_environment: 'production', duration_ms: 96, span_count: 7, error_count: 0, status_code: 'OK', start_time: '10:04:31', kind: 'SERVER' },
  { trace_id: 'd4e5f6a7b8c9d0e1', root_span_name: 'invoice.generate', service_name: 'billing-worker', deployment_environment: 'production', duration_ms: 2210, span_count: 18, error_count: 0, status_code: 'OK', start_time: '13:15:00', kind: 'CONSUMER' },
  { trace_id: 'e5f6a7b8c9d0e1f2', root_span_name: 'GET /healthz', service_name: 'api-gateway', deployment_environment: 'production', duration_ms: 3, span_count: 1, error_count: 0, status_code: 'OK', start_time: '16:42:59', kind: 'SERVER' },
  { trace_id: 'f6a7b8c9d0e1f2a3', root_span_name: 'POST /checkout', service_name: 'api-gateway', deployment_environment: 'staging', duration_ms: 240, span_count: 12, error_count: 0, status_code: 'OK', start_time: '18:03:10', kind: 'SERVER' },
  { trace_id: 'a7b8c9d0e1f2a3b4', root_span_name: 'GET /api/cart', service_name: 'api-gateway', deployment_environment: 'production', duration_ms: 61, span_count: 5, error_count: 0, status_code: 'OK', start_time: '09:12:48', kind: 'SERVER' },
]
const TRACE_STATE: Record<Trace['status_code'], State> = { OK: 'ok', ERROR: 'error', UNSET: 'idle' }
const OPS = [
  { span_name: 'POST /checkout', service_name: 'api-gateway', count: 1840, p50: 142, p95: 398, p99: 612, error_rate: 1.7, tail_ratio: 4.3 },
  { span_name: 'GET /api/products', service_name: 'api-gateway', count: 12400, p50: 48, p95: 210, p99: 1900, error_rate: 0.0, tail_ratio: 39.6 },
  { span_name: 'db.query SELECT products', service_name: 'api-gateway', count: 12400, p50: 9, p95: 41, p99: 1650, error_rate: 0.0, tail_ratio: 183 },
  { span_name: 'GET /api/cart', service_name: 'api-gateway', count: 9800, p50: 22, p95: 70, p99: 140, error_rate: 0.1, tail_ratio: 6.4 },
  { span_name: 'invoice.generate', service_name: 'billing-worker', count: 640, p50: 1800, p95: 2400, p99: 3100, error_rate: 0.3, tail_ratio: 1.7 },
  { span_name: 'redis.get cart', service_name: 'api-gateway', count: 22000, p50: 1, p95: 3, p99: 8, error_rate: 0.0, tail_ratio: 8 },
]
const LATENCY = Array.from({ length: 48 }, (_, i) => ({ t: `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`, p95: Math.round(150 + 40 * Math.sin(i / 5) + (i >= 41 ? 180 : 0) + (i % 7 === 0 ? 30 : 0)), p50: Math.round(60 + 10 * Math.sin(i / 4)) }))

const TRACE_TABS = ['traces', 'operations'] as const
export function TracesScreen({ go, dense, plan, notify }: { go: (v: string) => void; dense: boolean; plan: Plan; notify: Notify }) {
  const [tab, setTab] = useState<(typeof TRACE_TABS)[number]>('traces')
  const [q, setQ] = useState('')
  const [only, setOnly] = useState<'all' | 'errors' | 'slow'>('all')
  const [range, setRange] = useState('24h')
  const [hot, setHot] = useState<string | null>(null)
  // Chart selection narrows the ledger: a trace belongs to the half-hour bucket its start_time falls in.
  const [window_, setWindow] = useState<TimeRange | null>(null)
  const bucket = (hms: string) => `${hms.slice(0, 2)}:${Number(hms.slice(3, 5)) >= 30 ? '30' : '00'}`
  const idx = (t: string) => LATENCY.findIndex((p) => p.t === t)
  const inWindow = (t: Trace) => !window_ || (idx(bucket(t.start_time)) >= idx(window_.from) && idx(bucket(t.start_time)) <= idx(window_.to))
  const list = TRACES.filter((t) => inWindow(t) && matches(q, t.root_span_name, t.trace_id, t.service_name, t.deployment_environment) && (only === 'all' || (only === 'errors' && t.error_count > 0) || (only === 'slow' && t.duration_ms > 398)))
  const max = Math.max(...TRACES.map((t) => t.duration_ms))
  const rows: LedgerRow[] = list.map((t) => ({
    id: t.trace_id, state: TRACE_STATE[t.status_code], onOpen: () => go(`trace:${t.trace_id}`),
    mobile: <><span className="block font-medium">{t.root_span_name}</span><span className="block truncate text-[11px] text-muted-foreground">{t.service_name} · {t.duration_ms}ms · {t.start_time}</span></>,
    cells: [
      <span className="font-mono text-muted-foreground">{t.trace_id.slice(0, 8)}</span>,
      <span className="font-medium"><Status state={TRACE_STATE[t.status_code]} label={t.root_span_name} /></span>,
      <span className="text-muted-foreground">{t.service_name} <span className="text-[10px]">· {t.deployment_environment}</span></span>,
      <span className="flex items-center gap-2">{t.duration_ms > 398 ? <Status state="warn" label={`${t.duration_ms}ms`} /> : <Num value={t.duration_ms} unit="ms" />}<span className="h-1.5 flex-1 bg-muted"><span className="block h-full bg-foreground" style={{ width: `${(t.duration_ms / max) * 100}%` }} /></span></span>,
      <Num value={t.span_count} />,
      t.error_count ? <Status state="error" label={String(t.error_count)} /> : <Num value={null} />,
      <span className="font-mono tabular-nums text-muted-foreground">{t.start_time}</span>,
    ],
  }))
  const status = (
    <StatusLine state="warn" more={{ label: '+1 warning', items: [{ state: 'warn', children: <><Phrase onClick={() => setTab('operations')}>GET /api/products</Phrase> p99 is 40× its p50.</> }] }}>
      <Phrase onClick={() => setOnly('errors')}>31 errored traces</Phrase> <Phrase onClick={() => setWindow({ from: '20:30', to: '23:30' })}>since dep_91a</Phrase>, all POST /checkout.
    </StatusLine>
  )
  return (
    <Detail title="Traces" meta="acme-api · production · OpenTelemetry" status={status} tabs={TRACE_TABS} tab={tab} onTab={setTab}
      actions={<RangePicker ranges={RANGES} value={range} onChange={setRange} retentionDays={plan.retentionDays} retentionLabel={plan.retention} onGated={gated(notify, plan)} />}>
      {tab === 'traces' && (
        <div className="space-y-6">
          <Section title="Latency" meta="p95 and p50 · 30 min buckets">
            <div className="space-y-2">
            <TimeChart data={LATENCY} unit="ms" height={140} xInterval={11} series={[{ key: 'p95', name: 'p95' }, { key: 'p50', name: 'p50' }]} markers={[{ id: 'dep_90e', x: '10:00' }, { id: 'dep_91a', x: '20:30' }]} hot={hot} onHot={setHot} selection={window_} onSelect={setWindow} sampled={plan.sampled ? { from: '14:00', to: '23:30', label: 'sampled 1 in 4' } : undefined} readoutFormat={(p) => `${p.t} · p95 ${p.p95}ms · p50 ${p.p50}ms`} />
            <ChartFooter><span>showing {range}</span><span>· retention {plan.retention}</span><span>· ┆ deploy</span><span>· a selection narrows the traces below</span></ChartFooter>
            </div>
          </Section>
          <Ledger
            status={null}
            columns={['trace', 'root span', 'service', 'duration', 'spans', 'errors', 'start']} grid="90px 1.6fr 1.2fr 1.6fr 60px 60px 80px"
            rows={rows} total={TRACES.length} filter={q} onFilter={setQ} placeholder="filter by span, service or trace id" dense={dense}
            hint={window_ ? `${list.length} traces started between ${window_.from} and ${window_.to} · clear the selection on the chart to see all` : undefined}
            action={<Segmented options={[['all', 'all'], ['errors', 'errors'], ['slow', 'slower than p95']] as const} value={only} onChange={setOnly} />}
          />
        </div>
      )}
      {tab === 'operations' && (
        <div className="space-y-3">
          <p className="text-xs text-muted-foreground">Per span name over {range}. Tail ratio is p99 ÷ p50; anything above 10 means a few requests are very different from the rest, usually a missing index or a cold cache.</p>
          <div className="op-rows border">
            <div className="op-row op-cols hidden items-center md:grid" style={{ '--cols': '2fr 1fr 80px 80px 80px 80px 90px 90px' } as CSSProperties}>
              {['operation', 'service', 'count', 'p50', 'p95', 'p99', 'errors', 'tail ratio'].map((h) => <span key={h} className="op-label">{h}</span>)}
            </div>
            {[...OPS].sort((a, b) => b.tail_ratio - a.tail_ratio).map((o) => (
              <div key={o.span_name} className={cn('op-row op-cols grid grid-cols-[1fr_auto] items-center gap-x-3 text-xs', !dense && 'py-1 md:py-0')} style={{ '--cols': '2fr 1fr 80px 80px 80px 80px 90px 90px' } as CSSProperties}>
                <span className="truncate font-mono">{o.span_name}<span className="block text-[11px] text-muted-foreground md:hidden">{o.service_name} · p50 {o.p50}ms · p99 {o.p99}ms</span></span>
                <span className="hidden text-muted-foreground md:block">{o.service_name}</span>
                <span className="hidden md:block"><Num value={o.count} /></span>
                <span className="hidden md:block"><Num value={o.p50} unit="ms" /></span>
                <Num value={o.p95} unit="ms" />
                <span className="hidden md:block"><Num value={o.p99} unit="ms" /></span>
                <span className="hidden md:block">{o.error_rate > 1 ? <Status state="error" label={`${o.error_rate.toFixed(1)}%`} /> : <Num value={o.error_rate ? o.error_rate.toFixed(1) : null} unit="%" />}</span>
                <span className="hidden items-center gap-2 md:flex">{o.tail_ratio > 10 ? <Status state="warn" label={`${o.tail_ratio.toFixed(1)}×`} /> : <Num value={o.tail_ratio.toFixed(1)} unit="×" />}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </Detail>
  )
}

// ── Trace detail: waterfall ────────────────────────────────────────────

type Span = { span_id: string; parent_span_id: string | null; name: string; service: string; kind: 'SERVER' | 'CLIENT' | 'INTERNAL'; start: number; duration_ms: number; status_code: 'OK' | 'ERROR' | 'UNSET'; status_message: string; attributes: Record<string, string>; events: { name: string; at: number; attributes: Record<string, string> }[] }
const SPANS: Span[] = [
  { span_id: 's1', parent_span_id: null, name: 'POST /checkout', service: 'api-gateway', kind: 'SERVER', start: 0, duration_ms: 412, status_code: 'ERROR', status_message: 'TypeError: cannot read properties of undefined', attributes: { 'http.method': 'POST', 'http.route': '/checkout', 'http.status_code': '500', 'deployment.id': 'dep_91a', 'user.id': 'usr_2f9', 'session.id': 'sess_9f31c' }, events: [{ name: 'exception', at: 401, attributes: { 'exception.type': 'TypeError', 'exception.message': "cannot read properties of undefined (reading 'id')", 'code.filepath': 'src/checkout/AddressForm.tsx:88' }}] },
  { span_id: 's2', parent_span_id: 's1', name: 'auth.verify', service: 'api-gateway', kind: 'INTERNAL', start: 2, duration_ms: 9, status_code: 'OK', status_message: '', attributes: { 'auth.method': 'session' }, events: [] },
  { span_id: 's3', parent_span_id: 's1', name: 'redis.get cart', service: 'api-gateway', kind: 'CLIENT', start: 12, duration_ms: 2, status_code: 'OK', status_message: '', attributes: { 'db.system': 'redis', 'db.statement': 'GET cart:usr_2f9' }, events: [] },
  { span_id: 's4', parent_span_id: 's1', name: 'db.query SELECT addresses', service: 'api-gateway', kind: 'CLIENT', start: 16, duration_ms: 38, status_code: 'OK', status_message: '', attributes: { 'db.system': 'postgresql', 'db.statement': 'SELECT * FROM addresses WHERE user_id = $1', 'db.rows': '0' }, events: [] },
  { span_id: 's5', parent_span_id: 's1', name: 'address.normalize', service: 'api-gateway', kind: 'INTERNAL', start: 56, duration_ms: 3, status_code: 'ERROR', status_message: "TypeError: cannot read properties of undefined (reading 'id')", attributes: { 'code.function': 'normalizeAddress', 'code.filepath': 'src/checkout/AddressForm.tsx:88' }, events: [] },
  { span_id: 's6', parent_span_id: 's1', name: 'POST stripe /v1/payment_intents', service: 'api-gateway', kind: 'CLIENT', start: 60, duration_ms: 318, status_code: 'OK', status_message: '', attributes: { 'http.url': 'https://api.stripe.com/v1/payment_intents', 'http.status_code': '200', 'peer.service': 'stripe' }, events: [] },
  { span_id: 's7', parent_span_id: 's1', name: 'db.query INSERT orders', service: 'api-gateway', kind: 'CLIENT', start: 380, duration_ms: 14, status_code: 'OK', status_message: '', attributes: { 'db.system': 'postgresql', 'db.statement': 'INSERT INTO orders …' }, events: [] },
  { span_id: 's8', parent_span_id: 's1', name: 'email.send order_confirmation', service: 'api-gateway', kind: 'CLIENT', start: 396, duration_ms: 4, status_code: 'UNSET', status_message: '', attributes: { 'peer.service': 'temps-email', 'email.template': 'order_confirmation' }, events: [] },
]

export function TraceScreen({ id, go }: { id: string; go: (v: string) => void; dense?: boolean }) {
  const t = TRACES.find((x) => x.trace_id === id) ?? TRACES[0]
  const [sel, setSel] = useState<string>('s5')
  const total = SPANS[0].duration_ms
  const span = SPANS.find((s) => s.span_id === sel)!
  // The flat OTLP span list as the tree `Waterfall` draws; selection is the row's own focusable control, so `Tab` reaches it.
  const tree = useMemo(() => {
    const node = (sp: Span): VizSpan => ({ id: sp.span_id, name: sp.name, service: sp.service, start_ms: sp.start, duration_ms: sp.duration_ms, state: sp.status_code === 'ERROR' ? 'error' : sp.status_code === 'OK' ? 'ok' : 'idle', children: SPANS.filter((c) => c.parent_span_id === sp.span_id).map(node) })
    return SPANS.filter((sp) => !sp.parent_span_id).map(node)
  }, [])
  const errSpan = SPANS.find((s) => s.status_code === 'ERROR' && s.parent_span_id)!
  const stripe = SPANS.find((s) => s.name.includes('stripe'))!
  return (
    <Detail
      title={<span className="font-mono">{t.root_span_name}</span>}
      meta={`${t.trace_id} · ${t.service_name} · ${t.deployment_environment}`}
      status={
        <StatusLine state="error" more={{ label: `${Math.round((stripe.duration_ms / total) * 100)}% waiting on Stripe`, onClick: () => setSel(stripe.span_id) }}>
          Rebuild after dep_91a: <Phrase onClick={() => setSel(errSpan.span_id)}>{errSpan.name}</Phrase> threw at <span className="font-mono">src/checkout/AddressForm.tsx:88</span>.
        </StatusLine>
      }
      lede={
        <Lede state="error" word="failed" facts={[
          { k: 'duration', v: `${t.duration_ms}ms` },
          { k: 'spans', v: String(t.span_count) },
          { k: 'errors', v: String(t.error_count), state: t.error_count ? 'error' : undefined },
          { k: 'service', v: t.service_name },
          { k: 'start', v: t.start_time },
          { k: 'deploy', v: 'dep_91a' },
        ]}>
          {t.kind.toLowerCase()} span, HTTP 500 returned to the caller.
        </Lede>
      }
      actions={<>
        <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => go('issue:i_4821')}>open error</Button>
        <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => go('api-gateway')}>open dep_91a</Button>
      </>}
    >
      <Columns>
        <div>
          <Section title="Spans" meta={`${SPANS.length} · ${total}ms`}>
            <Waterfall spans={tree} total_ms={total} selected={sel} onSelect={(sp) => setSel(sp.id)} />
          </Section>
        </div>
        <div>
          {span.status_message && (
            <Section title="Failure">
              <Callout state="error" title={`${span.name} returned an error`} quote={span.status_message}>
                The root span returned HTTP 500 to the caller. The fix ships with the next build of {span.service}.
              </Callout>
            </Section>
          )}
          <Section title="Span" meta={`${span.name} · ${span.duration_ms}ms · ${span.kind.toLowerCase()} · +${span.start}ms`}>
            <KeyValue compact rows={Object.entries(span.attributes).map(([k, v]) => ({ k, v }))} />
          </Section>
          {span.events.length > 0 && (
            <Section title="Events" meta={String(span.events.length)}>
              <KeyValue compact rows={span.events.flatMap((e) => [{ k: e.name, v: `at ${e.at}ms` }, ...Object.entries(e.attributes).map(([k, v]) => ({ k, v }))])} />
            </Section>
          )}
        </div>
      </Columns>
    </Detail>
  )
}

// ── Metrics explorer ───────────────────────────────────────────────────

type MetricDef = { name: string; unit: string; kind: 'histogram' | 'gauge' | 'counter'; series: string[]; alert?: string }
const METRICS: MetricDef[] = [
  { name: 'http.server.request.duration', unit: 'ms', kind: 'histogram', series: ['http.route'], alert: 'p95 > 400ms for 5m' },
  { name: 'http.server.active_requests', unit: '', kind: 'gauge', series: ['http.route'] },
  { name: 'db.client.operation.duration', unit: 'ms', kind: 'histogram', series: ['db.operation'] },
  { name: 'process.runtime.memory', unit: 'MB', kind: 'gauge', series: [] },
  { name: 'process.cpu.utilization', unit: '%', kind: 'gauge', series: [] },
  { name: 'checkout.orders.completed', unit: '', kind: 'counter', series: ['payment.method'] },
  { name: 'queue.invoice.lag', unit: 's', kind: 'gauge', series: [], alert: '> 60s for 10m' },
]
function seriesFor(name: string) {
  const base = name.includes('duration') ? 150 : name.includes('memory') ? 512 : name.includes('cpu') ? 35 : name.includes('orders') ? 40 : name.includes('lag') ? 4 : 20
  return Array.from({ length: 48 }, (_, i) => {
    const t = `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`
    const wave = Math.max(0, Math.sin(((i / 2 - 7) / 24) * Math.PI * 2))
    const spike = name.includes('duration') && i >= 41 ? 180 : 0
    const avg = Math.round(base * (0.6 + wave * 0.8) + spike)
    return { t, avg, p95: Math.round(avg * 2.4), p50: Math.round(avg * 0.7), max: Math.round(avg * 3.2) }
  })
}
const BREAKDOWN = [['/checkout', 398, 1840], ['/api/products', 210, 12400], ['/api/cart', 70, 9800], ['/healthz', 3, 86400], ['/api/orders/:id', 120, 2100]] as const

export function MetricsScreen({ dense, plan, notify }: { dense: boolean; plan: Plan; notify: Notify }) {
  const [metric, setMetric] = useState(METRICS[0])
  const [q, setQ] = useState('')
  const [range, setRange] = useState('24h')
  const [agg, setAgg] = useState<'p95' | 'p50' | 'avg' | 'max'>('p95')
  const [pct, setPct] = useState<Pct>('p95')
  const hist = useMemo(() => [5, 10, 25, 50, 100, 250, 500, 1000, 2500].map((le, i) => ({ le, count: [40, 180, 620, 1450, 1720, 980, 310, 90, 22][i] * (metric.name.includes('db') ? 0.4 : 1) })), [metric])
  const [hot, setHot] = useState<string | null>(null)
  const data = useMemo(() => seriesFor(metric.name), [metric])
  const last = data[data.length - 1]
  const names = METRICS.filter((m) => matches(q, m.name, m.kind))
  return (
    <div className="space-y-6">
      <PageTitle title="Metrics" meta={`acme-api · production · ${METRICS.length} metrics`} />
      <StatusLine state="warn">
        <Phrase onClick={() => setMetric(METRICS[0])}>http.server.request.duration</Phrase> p95 is {last.p95}ms, above the 400ms alert.
      </StatusLine>
      <div className="grid gap-6 xl:grid-cols-[260px_minmax(0,1fr)]">
        {/* Metric names */}
        <div className="space-y-2">
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="filter metrics" aria-label="Filter metrics" className="h-8 w-full border bg-background px-2 text-xs" />
          <div className="op-rows border text-xs" role="listbox" aria-label="Metrics">
            {names.map((m) => (
              <button key={m.name} role="option" aria-selected={metric.name === m.name} onClick={() => setMetric(m)} className={cn('flex w-full items-center gap-2 px-2 text-left hover:bg-muted', dense ? 'h-7' : 'h-8', metric.name === m.name && 'bg-foreground text-background hover:bg-foreground')}>
                <span className="w-3 text-center font-mono text-[10px] opacity-70">{m.kind === 'histogram' ? 'H' : m.kind === 'gauge' ? 'G' : 'C'}</span>
                <span className="min-w-0 flex-1 truncate font-mono">{m.name}</span>
                {m.alert && <span aria-hidden className={cn('text-warning', metric.name === m.name && 'text-background')}>◐</span>}
              </button>
            ))}
            {names.length === 0 && <p className="px-2 py-3 text-muted-foreground">no metric matches "{q}"</p>}
          </div>
          <p className="text-[11px] text-muted-foreground">H histogram · G gauge · C counter · ◐ has an alert rule</p>
        </div>
        {/* Chart + breakdown */}
        <div className="min-w-0 space-y-6">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-sm">{metric.name}</span>
              <span className="text-[11px] text-muted-foreground">{metric.kind}{metric.unit && ` · ${metric.unit}`}{metric.alert && ` · alert ${metric.alert}`}</span>
              <div className="flex w-full flex-wrap gap-2 sm:ml-auto sm:w-auto">
                {metric.kind === 'histogram' && <Segmented options={[['p95', 'p95'], ['p50', 'p50'], ['avg', 'avg'], ['max', 'max']] as const} value={agg} onChange={setAgg} />}
                <RangePicker ranges={RANGES} value={range} onChange={setRange} retentionDays={plan.retentionDays} retentionLabel={plan.retention} onGated={gated(notify, plan)} />
              </div>
            </div>
            <TimeChart data={data} unit={metric.unit} height={200} xInterval={11}
              series={metric.kind === 'histogram' ? [{ key: agg, name: agg }, { key: 'p50', name: 'p50' }].filter((s, i, a) => a.findIndex((x) => x.key === s.key) === i) : [{ key: 'avg', name: metric.name }]}
              markers={[{ id: 'dep_90e', x: '10:00' }, { id: 'dep_91a', x: '20:30' }]} hot={hot} onHot={setHot}
              sampled={plan.sampled ? { from: '14:00', to: '23:30', label: 'sampled 1 in 4' } : undefined}
              readoutFormat={(p) => `${p.t} · ${metric.kind === 'histogram' ? `${agg} ${p[agg]}${metric.unit}` : `${p.avg}${metric.unit}`}`} />
            <ChartFooter><span>showing {range}</span><span>· retention {plan.retention}</span><span>· ┆ deploy</span>{metric.alert && <span className="text-warning">· ◐ alert {metric.alert}</span>}</ChartFooter>
          </div>
          {metric.kind === 'histogram' && (
            <Section title="Distribution" meta={`${range} · ${metric.unit} · bucket upper bounds`}>
              <Histogram buckets={hist} unit={metric.unit} value={pct} onChange={setPct} />
            </Section>
          )}
          <MetricGrid cols={4}>
            <Metric label="now" value={metric.kind === 'histogram' ? last[agg] : last.avg} unit={metric.unit} baseline="latest 30 min bucket" state={metric.alert && metric.kind === 'histogram' && last.p95 > 400 ? 'warn' : 'ok'} />
            <Metric label="vs before dep_91a" value={metric.kind === 'histogram' ? `+${last.p95 - data[40].p95}` : `${last.avg - data[40].avg}`} unit={metric.unit} baseline="same aggregate at 20:00" state={metric.kind === 'histogram' && last.p95 - data[40].p95 > 100 ? 'warn' : 'ok'} />
            <Metric label="24h max" value={Math.max(...data.map((d) => d.max))} unit={metric.unit} baseline="single bucket" />
            <Metric label="data points" value={48 * (metric.series.length ? BREAKDOWN.length : 1)} baseline={`${metric.series.length ? BREAKDOWN.length : 1} series · 30 min buckets`} />
          </MetricGrid>
          {metric.series.length > 0 ? (
            <div className="op-rows border">
              <div className="op-row op-cols hidden items-center md:grid" style={{ '--cols': '1.6fr 1fr 100px 100px' } as CSSProperties}>{[metric.series[0], '', agg, 'count'].map((h, i) => <span key={i} className="op-label">{h}</span>)}</div>
              {BREAKDOWN.map(([k, p95, n]) => (
                <div key={k} className={cn('op-row op-cols grid grid-cols-[1fr_auto] items-center gap-x-3 text-xs', !dense && 'py-1 md:py-0')} style={{ '--cols': '1.6fr 1fr 100px 100px' } as CSSProperties}>
                  <span className="font-mono">{k}<span className="ml-2 text-[11px] text-muted-foreground md:hidden">{n.toLocaleString()} req</span></span>
                  <span className="hidden h-1.5 bg-muted md:block"><span className="block h-full bg-foreground" style={{ width: `${(p95 / 400) * 100}%` }} /></span>
                  {p95 > 380 ? <Status state="warn" label={`${p95}${metric.unit}`} /> : <Num value={p95} unit={metric.unit} />}
                  <span className="hidden md:block"><Num value={n} /></span>
                </div>
              ))}
            </div>
          ) : (
            <PageState state="empty" title="No series breakdown" reason={`${metric.name} has no attributes to group by. Gauges from the runtime are one series per instance.`} />
          )}
        </div>
      </div>
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { ExternalLink, Play, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  Callout, Columns, Detail, EchoDialog, Field, GitProviderLogo, KeyValue, Ledger, Lede, Metric, MetricGrid, Num, PageState, Phrase, Picker, Section, Segmented, Settings, Status, StatusLine,
  type LedgerRow, type State, type StatusItem } from '@/components/op'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   Backups · Git providers · Security on v5, from the real console's shapes:
   S3SourceResponse, BackupScheduleResponse, BackupResponse (state,
   current_step, attempts/max_attempts, live_size_bytes), ConnectionResponse
   (health_status, is_expired, synced_repository_count, installation_id),
   ScanResponse (critical/high/medium/low counts, scanner_type, commit),
   VulnerabilityResponse, SecurityHeadersSettings and SecurityConfig
   (passwordProtection, rateLimiting, geoRestrictions, attackMode).

   What changes against temps/web today:
   - Backups is one screen with three tabs (schedules, backups, sources);
     the live page is only the S3-sources table and the alerts live in a
     header bell. The verdict comes first: which job failed, which schedule
     is overdue.
   - A running backup shows its engine step and live size in the row, not
     behind a detail page.
   - Git providers lead with connection health: an expired installation is
     the status line, with reconnect as the one action.
   - Security merges scans, headers and access rules under one title; each
     environment's last scan is a row, the counts are the cells, and the
     status line names the worst one.
   ──────────────────────────────────────────────────────────────────────── */

type Notify = (level: 'ok' | 'warn' | 'err', msg: string, detail?: string) => void
type Plan = { label: string; pitr: string }

/** Switch plus the word. A toggle alone reads as a shape; "on"/"off" reads as a state. */
export function Toggle({ checked, onChange, disabled }: { checked: boolean; onChange: (v: boolean) => void; disabled?: boolean }) {
  return (
    <span className="inline-flex items-center gap-2">
      <Switch checked={checked} onCheckedChange={onChange} disabled={disabled} />
      <span className={cn('font-mono text-xs', checked ? 'text-foreground' : 'text-muted-foreground')}>{checked ? 'on' : 'off'}</span>
    </span>
  )
}

// ── Backups ────────────────────────────────────────────────────────────

const SOURCES = [
  { id: 1, name: 'hetzner-fsn1', bucket_name: 'acme-backups', bucket_path: 'prod/', region: 'fsn1', endpoint: 'https://fsn1.your-objectstorage.com', is_default: true, force_path_style: true, schedules: 2, used: '212 GB' },
  { id: 2, name: 'r2-offsite', bucket_name: 'acme-offsite', bucket_path: '', region: 'auto', endpoint: 'https://a1b2.r2.cloudflarestorage.com', is_default: false, force_path_style: false, schedules: 1, used: '48 GB' },
]
const SCHEDULES = [
  { id: 1, name: 'nightly-all', enabled: true, backup_type: 'scheduled', schedule_expression: '0 2 * * *', human: 'daily 02:00', targets: 'all services + control plane', target_all_services: true, include_control_plane: true, retention_period: 30, s3: 'hetzner-fsn1', last_run: '02:00 today', next_run: 'in 17h', state: 'error' as State, note: 'events-ch failed, retrying' },
  { id: 2, name: 'pg-hourly', enabled: true, backup_type: 'scheduled', schedule_expression: '0 * * * *', human: 'hourly', targets: 'acme-pg', target_all_services: false, include_control_plane: false, retention_period: 7, s3: 'hetzner-fsn1', last_run: '41m ago', next_run: 'in 19m', state: 'ok' as State, note: '' },
  { id: 3, name: 'weekly-offsite', enabled: true, backup_type: 'scheduled', schedule_expression: '0 4 * * 0', human: 'sundays 04:00', targets: 'acme-pg, billing-maria, catalog-mongo', target_all_services: false, include_control_plane: true, retention_period: 90, s3: 'r2-offsite', last_run: '9d ago', next_run: 'overdue by 2d', state: 'warn' as State, note: 'overdue by 2d · last run failed to start' },
  { id: 4, name: 'redis-snapshots', enabled: false, backup_type: 'scheduled', schedule_expression: '*/30 * * * *', human: 'every 30 min', targets: 'sessions-redis', target_all_services: false, include_control_plane: false, retention_period: 2, s3: 'hetzner-fsn1', last_run: '3w ago', next_run: 'paused', state: 'idle' as State, note: 'paused' },
]
const BACKUPS = [
  { id: 918, backup_id: 'bk_918', name: 'nightly-all', service: 'events-ch', engine: 'clickhouse', state: 'running', current_step: 'upload_parts', attempts: 2, max_attempts: 3, live_size_bytes: 21.4, size_bytes: null, started_at: '02:00', completed_at: null, expires_at: 'in 30d', error: null },
  { id: 917, backup_id: 'bk_917', name: 'nightly-all', service: 'acme-pg', engine: 'postgres', state: 'completed', current_step: null, attempts: 1, max_attempts: 3, live_size_bytes: null, size_bytes: 4.2, started_at: '02:00', completed_at: '02:06', expires_at: 'in 30d', error: null },
  { id: 916, backup_id: 'bk_916', name: 'nightly-all', service: 'control plane', engine: 'postgres', state: 'completed', current_step: null, attempts: 1, max_attempts: 3, live_size_bytes: null, size_bytes: 0.31, started_at: '02:00', completed_at: '02:01', expires_at: 'in 30d', error: null },
  { id: 915, backup_id: 'bk_915', name: 'nightly-all', service: 'billing-maria', engine: 'mariadb', state: 'completed', current_step: null, attempts: 1, max_attempts: 3, live_size_bytes: null, size_bytes: 1.1, started_at: '02:00', completed_at: '02:03', expires_at: 'in 30d', error: null },
  { id: 914, backup_id: 'bk_914', name: 'pg-hourly', service: 'acme-pg', engine: 'postgres', state: 'completed', current_step: null, attempts: 1, max_attempts: 3, live_size_bytes: null, size_bytes: 4.2, started_at: '01:00', completed_at: '01:05', expires_at: 'in 7d', error: null },
  { id: 909, backup_id: 'bk_909', name: 'nightly-all', service: 'events-ch', engine: 'clickhouse', state: 'failed', current_step: 'upload_parts', attempts: 3, max_attempts: 3, live_size_bytes: null, size_bytes: null, started_at: 'yesterday 02:00', completed_at: null, expires_at: '–', error: 'S3 PutObject 403 on part 412: signature expired after 3600s (rotate the hetzner-fsn1 access key or raise part size)' },
  { id: 902, backup_id: 'bk_902', name: 'weekly-offsite', service: 'catalog-mongo', engine: 'mongodb', state: 'completed', current_step: null, attempts: 1, max_attempts: 3, live_size_bytes: null, size_bytes: 6.8, started_at: '9d ago', completed_at: '9d ago', expires_at: 'in 81d', error: null },
]
const BK_STATE: Record<string, State> = { running: 'warn', completed: 'ok', failed: 'error', pending: 'idle', cancelled: 'idle' }
const BK_TABS = ['schedules', 'backups', 'sources'] as const

/** Case-insensitive filter over several fields; every ledger filter in the console uses this so "API" finds api-gateway. */
export function matches(q: string, ...fields: (string | undefined | null)[]) { const n = q.trim().toLowerCase(); return !n || fields.some((f) => (f ?? '').toLowerCase().includes(n)) }
/** "212 GB" → 212, "1.2 TB" → 1228.8: sizes sort as numbers. */
function sizeNum(v: string) { const m = v.match(/([\d.]+)\s*(GB|TB|MB)/i); if (!m) return Number.NaN; const n = Number(m[1]); return m[2].toUpperCase() === 'TB' ? n * 1024 : m[2].toUpperCase() === 'MB' ? n / 1024 : n }
/** Ordinal for the schedule's next run so the column sorts by time, not by id. */
const NEXT_RUN_ORDER: Record<string, number> = { 'in 12m': 1, 'in 41m': 2, 'in 2h': 3, 'in 3h': 4, 'in 6h': 5, 'tonight 02:00': 6, 'tomorrow 02:00': 7, 'Sunday 03:00': 8 }

export function BackupsScreen({ dense, plan, notify, go }: { dense: boolean; plan: Plan; notify: Notify; go: (v: string) => void }) {
  const [tab, setTab] = useState<(typeof BK_TABS)[number]>('schedules')
  const [q, setQ] = useState('')
  const [backups, setBackups] = useState(BACKUPS)
  const [sources, setSources] = useState(SOURCES)
  const failed = backups.find((b) => b.state === 'failed')
  const overdue = SCHEDULES.find((s) => s.state === 'warn')
  const items: StatusItem[] = []
  if (overdue) items.push({ state: 'warn', children: <><Phrase onClick={() => setTab('schedules')}>{overdue.name}</Phrase> is {overdue.next_run}.</> })
  const status = (
    <StatusLine state={failed ? 'error' : overdue ? 'warn' : 'ok'} more={items.length ? { label: `+${items.length} warning${items.length > 1 ? 's' : ''}`, items } : undefined}>
      {failed ? <><Phrase onClick={() => setTab('backups')}>{failed.name}</Phrase> failed on {failed.service} after {failed.attempts} attempts.</> : overdue ? <>{overdue.name} is {overdue.next_run}.</> : <>All schedules ran on time.</>}
    </StatusLine>
  )

  const scheduleRows: LedgerRow[] = SCHEDULES.filter((s) => matches(q, s.name, s.targets)).map((s) => ({
    id: s.name, state: s.state, onOpen: () => notify('ok', `open schedule ${s.name}`),
    sort: { name: s.name, retention: s.retention_period, next: s.next_run === 'paused' ? null : NEXT_RUN_ORDER[s.next_run] ?? 999 },
    mobile: <><span className="block truncate font-medium">{s.name}</span><span className="block truncate text-[11px] text-muted-foreground">{s.note || `${s.human} · next ${s.next_run}`}</span></>,
    cells: [
      <span className="font-medium">{s.name}</span>,
      <Status state={s.state} label={s.note || 'on schedule'} />,
      <span className="truncate text-muted-foreground">{s.targets}</span>,
      <span className="font-mono">{s.human}</span>,
      <span className="text-muted-foreground">{s.next_run}</span>,
      <Num value={s.retention_period} unit="d" />,
      <span className="text-muted-foreground">{s.s3}</span>,
    ],
  }))

  const backupRows: LedgerRow[] = backups.filter((b) => matches(q, b.name, b.service, b.backup_id, b.engine, b.state)).map((b) => ({
    id: b.backup_id, state: BK_STATE[b.state], onOpen: () => notify('ok', `open ${b.backup_id}`),
    sort: { id: b.id, service: b.service, size: b.size_bytes ?? b.live_size_bytes ?? null },
    mobile: <><span className="block truncate font-medium">{b.backup_id} · {b.service}</span><span className="block truncate text-[11px] text-muted-foreground">{b.state === 'running' ? `${b.current_step} · ${b.live_size_bytes} GB so far` : b.error ?? `${b.name} · ${b.started_at}`}</span></>,
    cells: [
      <span className="font-mono">{b.backup_id}</span>,
      <Status state={BK_STATE[b.state]} label={b.state === 'running' ? `${b.current_step} · attempt ${b.attempts}/${b.max_attempts}` : b.state === 'failed' ? `failed · ${b.attempts}/${b.max_attempts} attempts` : b.state} />,
      <span className="font-medium">{b.service} <span className="font-normal text-muted-foreground">{b.engine}</span></span>,
      <span className="text-muted-foreground">{b.name}</span>,
      b.state === 'running' ? <span className="font-mono tabular-nums text-muted-foreground">{b.live_size_bytes} GB…</span> : <Num value={b.size_bytes ?? null} unit=" GB" />,
      <span className="text-muted-foreground">{b.started_at}</span>,
      <span className="text-muted-foreground">{b.expires_at}</span>,
    ],
  }))

  const sourceRows: LedgerRow[] = sources.filter((s) => matches(q, s.name, s.bucket_name)).map((s) => ({
    id: s.name, state: 'ok', onOpen: () => notify('ok', `open source ${s.name}`),
    sort: { name: s.name, used: sizeNum(s.used) },
    mobile: <><span className="block truncate font-medium">{s.name}{s.is_default && <span className="ml-2 border px-1 text-[10px] font-normal text-muted-foreground">default</span>}</span><span className="block truncate text-[11px] text-muted-foreground">s3://{s.bucket_name}/{s.bucket_path} · {s.region}</span>{!s.is_default && <span className="mt-1 block text-[11px]"><EchoDialog trigger={<a href="#" onClick={(e) => { e.preventDefault(); e.stopPropagation() }}>make default</a>} echo={`$ temps backup source default ${s.name}`} title="Make default source" description={`New schedules and manual backups go to ${s.name}. Existing schedules keep their source.`} confirmWord={s.name} steps={['verify bucket access', 'switch default']} onDone={() => { setSources((p) => p.map((x) => ({ ...x, is_default: x.id === s.id }))); notify('ok', `${s.name} is the default source`) }} /></span>}</>,
    cells: [
      <span className="font-medium">{s.name}{s.is_default && <span className="ml-2 border px-1 text-[10px] font-normal text-muted-foreground">default</span>}</span>,
      <span className="font-mono">s3://{s.bucket_name}/{s.bucket_path}</span>,
      <span className="text-muted-foreground">{s.region}</span>,
      <span className="truncate font-mono text-muted-foreground">{s.endpoint.replace('https://', '')}</span>,
      <Num value={s.schedules} />,
      <span className="font-mono">{s.used}</span>,
      s.is_default ? <span className="text-muted-foreground">—</span> : (
        <EchoDialog trigger={<a href="#" onClick={(e) => { e.preventDefault(); e.stopPropagation() }}>make default</a>} echo={`$ temps backup source default ${s.name}`} title="Make default source" description={`New schedules and manual backups go to ${s.name}. Existing schedules keep their source.`} confirmWord={s.name} steps={['verify bucket access', 'switch default']} onDone={() => { setSources((p) => p.map((x) => ({ ...x, is_default: x.id === s.id }))); notify('ok', `${s.name} is the default source`) }} />
      ),
    ],
  }))

  return (
    <Detail title="Backups" meta={`${SCHEDULES.filter((s) => s.enabled).length} schedules · ${sources.length} sources · point-in-time recovery ${plan.pitr}`} status={status} tabs={BK_TABS} tab={tab} onTab={(t) => { setTab(t); setQ('') }}
      actions={<>
        {tab === 'schedules' && <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => notify('ok', 'nightly-all started', 'events-ch first, then the rest')}><Play /> run nightly-all now</Button>}
        {tab === 'schedules' && <Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'new schedule')}><Plus /> new schedule</Button>}
        {tab === 'backups' && <Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'backup started', 'acme-pg → hetzner-fsn1')}><Play /> back up now</Button>}
        {tab === 'sources' && <Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'add source')}><Plus /> add source</Button>}
      </>}>
      {tab === 'schedules' && (
        <Ledger status={null} columns={[{ label: 'schedule', key: 'name' }, 'status', 'targets', 'runs', { label: 'next run', key: 'next' }, { label: 'keeps', key: 'retention', numeric: true }, 'source']} grid="1fr 1.8fr 1.5fr minmax(80px,max-content) minmax(80px,max-content) minmax(55px,max-content) minmax(80px,max-content)"
          rows={scheduleRows} total={SCHEDULES.length} filter={q} onFilter={setQ} placeholder="filter schedules" hint="needs attention first" dense={dense}
          footer={<>{scheduleRows.length} of {SCHEDULES.length} · a schedule with all services on also picks up services created later · retention is per schedule, PITR is per plan ({plan.pitr})</>} />
      )}
      {tab === 'backups' && (
        <div className="space-y-6">
          {failed && (
            <Callout state="error" title={`${failed.backup_id} failed on ${failed.service} after ${failed.attempts} attempts`} quote={failed.error}
              action={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', `retrying ${failed.name}`, `${failed.service} only`)}>retry {failed.service}</Button>}>
              Step <span className="font-mono">{failed.current_step}</span> never completed, so {failed.service} has no backup since yesterday. Rotate the access key on <Phrase onClick={() => setTab('sources')}>hetzner-fsn1</Phrase> first, or the retry fails the same way.
            </Callout>
          )}
          <MetricGrid cols={4}>
            <Metric label="last 24h" value={6} baseline="5 completed · 1 running" state="ok" />
            <Metric label="failed · 7d" value={1} baseline="bk_909 · events-ch · S3 403" state="error" />
            <Metric label="stored" value="260" unit=" GB" baseline="hetzner-fsn1 212 · r2-offsite 48" />
            <Metric label="oldest restorable" value="81" unit="d" baseline="weekly-offsite · catalog-mongo" />
          </MetricGrid>
          <Ledger status={null} columns={[{ label: 'backup', key: 'id' }, 'status', { label: 'service', key: 'service' }, 'schedule', { label: 'size', key: 'size', numeric: true }, 'started', 'expires']} grid="minmax(70px,max-content) 2fr 1.4fr minmax(80px,max-content) minmax(80px,max-content) minmax(60px,max-content) minmax(60px,max-content)"
            rows={backupRows} total={backups.length} filter={q} onFilter={setQ} placeholder="filter by service, schedule or id" hint="running and failed first" dense={dense}
            action={failed && (
              <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> delete {failed.backup_id}</Button>} echo={`$ temps backup delete ${failed.backup_id}`} title="Delete failed backup" description="Removes the partial upload from hetzner-fsn1. The service itself is untouched; the next nightly-all run will retry events-ch." confirmWord={failed.backup_id} steps={['abort multipart upload', 'remove S3 prefix', 'delete record']} onDone={() => { setBackups((p) => p.filter((b) => b.id !== failed.id)); notify('warn', `${failed.backup_id} deleted`) }} />
            )}
            footer={<>{backupRows.length} of {backups.length} · sizes are final on completion, partial while running · <Phrase onClick={() => notify('ok', 'restore', 'pick a backup row, then restore')}>restore</Phrase> from any completed row</>} />
        </div>
      )}
      {tab === 'sources' && (
        <Ledger status={null} columns={[{ label: 'source', key: 'name' }, 'bucket', 'region', 'endpoint', 'schedules', { label: 'stored', key: 'used', numeric: true }, '']} grid="1fr 1.6fr minmax(50px,max-content) 1.6fr minmax(70px,max-content) minmax(70px,max-content) minmax(90px,max-content)"
          rows={sourceRows} total={sources.length} filter={q} onFilter={setQ} placeholder="filter sources" dense={dense}
          footer={<>{sourceRows.length} of {sources.length} · credentials are encrypted at rest and never shown again · <Phrase onClick={() => go('databases')}>databases</Phrase> lists what each source protects</>} />
      )}
    </Detail>
  )
}

// ── Git providers ──────────────────────────────────────────────────────

const PROVIDERS = [
  // ProviderResponse: name, provider_type, auth_method (app | token | oauth), base_url for self-hosted, is_active, is_default
  { id: 1, name: 'github-acme', provider_type: 'github', kind: 'GitHub App', auth_method: 'app', base_url: 'github.com', is_active: true, is_default: true, created_at: '2025-11-02', connections: 2, repos: 34, state: 'error' as State, note: 'acme-org installation token expired' },
  { id: 2, name: 'gitlab-self-hosted', provider_type: 'gitlab', kind: 'GitLab', auth_method: 'token', base_url: 'gitlab.acme.internal', is_active: true, is_default: false, created_at: '2026-01-14', connections: 1, repos: 6, state: 'ok' as State, note: '' },
  { id: 3, name: 'gitea-lab', provider_type: 'gitea', kind: 'Gitea', auth_method: 'token', base_url: 'git.lab.acme.sh', is_active: true, is_default: false, created_at: '2026-06-30', connections: 0, repos: 0, state: 'idle' as State, note: 'no connection yet' },
  { id: 5, name: 'bitbucket-mobile', provider_type: 'bitbucket', kind: 'Bitbucket', auth_method: 'oauth', base_url: 'bitbucket.org', is_active: true, is_default: false, created_at: '2026-08-21', connections: 1, repos: 4, state: 'ok' as State, note: '' },
  { id: 4, name: 'github-legacy', provider_type: 'github', kind: 'GitHub', auth_method: 'token', base_url: 'github.com', is_active: false, is_default: false, created_at: '2025-03-19', connections: 1, repos: 2, state: 'idle' as State, note: 'inactive · token revoked' },
]
const CONNECTIONS = [
  { id: 11, provider_id: 1, account_name: 'acme-org', account_type: 'Organization', installation_id: '48211903', health_status: 'expired', is_expired: true, health_message: 'GitHub returned 401: installation access token expired; the app was suspended by an org admin on 2026-09-03', consecutive_health_failures: 6, synced_repository_count: 31, last_synced_at: '2d ago', is_active: true },
  { id: 12, provider_id: 1, account_name: 'maya', account_type: 'User', installation_id: '48211951', health_status: 'healthy', is_expired: false, health_message: null, consecutive_health_failures: 0, synced_repository_count: 3, last_synced_at: '6m ago', is_active: true },
  { id: 21, provider_id: 2, account_name: 'platform', account_type: 'Group', installation_id: null, health_status: 'healthy', is_expired: false, health_message: null, consecutive_health_failures: 0, synced_repository_count: 6, last_synced_at: '12m ago', is_active: true },
]
const CONN_STATE: Record<string, State> = { healthy: 'ok', degraded: 'warn', expired: 'error', unknown: 'idle' }

export function GitProvidersScreen({ dense, go }: { dense: boolean; go: (v: string) => void }) {
  const [q, setQ] = useState('')
  const bad = PROVIDERS.find((p) => p.state === 'error')
  const rows: LedgerRow[] = PROVIDERS.filter((p) => matches(q, p.name, p.kind, p.base_url, p.auth_method)).map((p) => ({
    id: p.name, state: p.state, onOpen: () => go(`git:${p.id}`),
    sort: { name: p.name, repos: p.repos || null, connections: p.connections, added: p.created_at },
    mobile: <><span className="flex items-center gap-2 truncate font-medium"><GitProviderLogo type={p.provider_type} className="text-muted-foreground" />{p.name}{p.is_default && <span className="ml-2 border px-1 text-[10px] font-normal text-muted-foreground">default</span>}</span><span className="block truncate text-[11px] text-muted-foreground">{p.note || `${p.kind} · ${p.auth_method === 'app' ? 'app' : 'token'} · ${p.base_url}`}</span></>,
    cells: [
      <span className="flex min-w-0 items-center gap-2 font-medium"><GitProviderLogo type={p.provider_type} className="text-muted-foreground" /><span className="truncate">{p.name}</span>{p.is_default && <span className="border px-1 text-[10px] font-normal text-muted-foreground">default</span>}</span>,
      <Status state={p.state} label={p.note || 'connected'} />,
      <span className="truncate text-muted-foreground">{p.kind} <span className="font-mono">{p.auth_method === 'app' ? 'app' : 'token'}</span>{p.base_url !== 'github.com' && <span className="font-mono"> · {p.base_url}</span>}</span>,
      <Num value={p.connections || null} />,
      <Num value={p.repos || null} />,
      <span className="font-mono text-muted-foreground">{p.created_at}</span>,
    ],
  }))
  return (
    <Ledger title="Git providers" meta={`${PROVIDERS.length} providers · ${PROVIDERS.reduce((n, p) => n + p.repos, 0)} repositories`}
      status={<StatusLine state={bad ? 'error' : 'ok'}>{bad ? <><Phrase onClick={() => go(`git:${bad.id}`)}>{bad.name}</Phrase>: acme-org installation token expired.</> : <>All providers healthy.</>}</StatusLine>}
      columns={[{ label: 'provider', key: 'name' }, 'status', 'auth · host', { label: 'connections', key: 'connections', numeric: true }, { label: 'repos', key: 'repos', numeric: true }, { label: 'added', key: 'added' }]} grid="1.2fr 1.8fr 1.4fr minmax(90px,max-content) minmax(60px,max-content) minmax(90px,max-content)"
      rows={rows} total={PROVIDERS.length} filter={q} onFilter={setQ} placeholder="filter providers" hint="needs attention first" dense={dense}
      action={<Button size="sm" className="op-primary h-8 text-xs"><Plus /> add provider</Button>}
      footer={<>{rows.length} of {PROVIDERS.length} · the default provider is offered first when connecting a repository · token providers cannot register webhooks themselves, an app can</>} />
  )
}

const GP_TABS = ['overview', 'connections', 'settings'] as const

export function GitProviderScreen({ id, dense, notify, go }: { id: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const p0 = PROVIDERS.find((p) => String(p.id) === id)
  const [tab, setTab] = useState<(typeof GP_TABS)[number]>('overview')
  const [conns, setConns] = useState(CONNECTIONS)
  const [cq, setCq] = useState('')
  const [form, setForm] = useState({ default: p0?.is_default ?? false, autoDeploy: true })
  const [saved, setSaved] = useState(form)
  if (!p0) return <PageState state="empty" title="No such provider" reason={`${id} is not configured.`} next={<Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => go('git')}>back to git providers</Button>} />
  const mine = conns.filter((c) => c.provider_id === p0.id)
  const expired = mine.find((c) => c.is_expired)
  const reconnect = () => { setConns((cs) => cs.map((c) => (c.id === expired?.id ? { ...c, is_expired: false, health_status: 'healthy', health_message: null, consecutive_health_failures: 0, last_synced_at: 'now' } : c))); notify('ok', 'acme-org reconnected', '31 repositories synced') }
  const status = (
    <StatusLine state={expired ? 'error' : mine.length === 0 ? 'idle' : 'ok'}>
      {expired ? <><Phrase onClick={reconnect}>Reconnect {expired.account_name}</Phrase>: its installation token expired {expired.last_synced_at.replace(' ago', '')} ago.</> : mine.length === 0 ? <>{p0.name} has no connection yet.</> : <>All {mine.length} connections healthy.</>}
    </StatusLine>
  )
  const connRows: LedgerRow[] = mine.filter((c) => matches(cq, c.account_name, c.account_type, c.installation_id, c.health_status)).map((c) => ({
    id: String(c.id), state: CONN_STATE[c.health_status], onOpen: () => notify('ok', `open ${c.account_name}`),
    sort: { account: c.account_name, repos: c.synced_repository_count },
    mobile: <><span className="block truncate font-medium">{c.account_name} <span className="font-normal text-muted-foreground">{c.account_type}</span></span><span className="block truncate text-[11px] text-muted-foreground">{c.is_expired ? 'token expired' : `${c.synced_repository_count} repos · synced ${c.last_synced_at}`}</span><span className="mt-1 block text-[11px]">{c.is_expired ? <a href="#" onClick={(e) => { e.preventDefault(); e.stopPropagation(); reconnect() }}>reconnect</a> : <a href="#" onClick={(e) => { e.preventDefault(); e.stopPropagation(); notify('ok', `syncing ${c.account_name}`) }}>sync now</a>}</span></>,
    cells: [
      <span className="font-medium">{c.account_name}</span>,
      <Status state={CONN_STATE[c.health_status]} label={c.is_expired ? `expired · ${c.consecutive_health_failures} failed checks` : c.health_status} />,
      <span className="text-muted-foreground">{c.account_type}</span>,
      <span className="font-mono text-muted-foreground">{c.installation_id ?? '—'}</span>,
      <Num value={c.synced_repository_count} />,
      <span className="text-muted-foreground">{c.last_synced_at}</span>,
      c.is_expired ? <a href="#" onClick={(e) => { e.preventDefault(); e.stopPropagation(); reconnect() }}>reconnect</a> : <a href="#" onClick={(e) => { e.preventDefault(); e.stopPropagation(); notify('ok', `syncing ${c.account_name}`) }}>sync now</a>,
    ],
  }))

  if (mine.length === 0) {
    return (
      <Detail title={p0.name} mark={<GitProviderLogo type={p0.provider_type} className="h-5 w-5" />} meta={`${p0.kind} · added ${p0.created_at}`} status={status} tabs={GP_TABS} tab={tab} onTab={setTab}>
        <PageState state="unconfigured" title="Installation required" missing={`a ${p0.kind} connection: authorize Temps on the account that owns the repositories you want to deploy.`} settingsHref="/settings" settingsLabel="connect an account"
          example={<div className="op-rows border text-xs"><div className="op-row flex items-center gap-3"><Status state="ok" label="acme-lab" /><span className="text-muted-foreground">Organization · 12 repos · synced 2m ago</span></div><div className="op-row flex items-center gap-3"><Status state="ok" label="maya" /><span className="text-muted-foreground">User · 3 repos · synced 2m ago</span></div></div>} />
      </Detail>
    )
  }

  if (tab === 'settings') {
    const dirty = form.default !== saved.default || form.autoDeploy !== saved.autoDeploy
    return (
      <Detail title={p0.name} mark={<GitProviderLogo type={p0.provider_type} className="h-5 w-5" />} meta={`${p0.kind} · added ${p0.created_at}`} status={status} tabs={GP_TABS} tab={tab} onTab={setTab}>
        <Settings status={null} dirty={dirty} onSave={() => { setSaved(form); notify('ok', 'provider settings saved', p0.name) }}
          sections={[
            { title: 'defaults', body: <>
              <Field label="default provider" help="offered first when connecting a repository to a project"><Toggle checked={form.default} onChange={(v) => setForm({ ...form, default: v })} /></Field>
              <Field label="auto-deploy new repos" help="repositories connected from this provider deploy on push to their default branch"><Toggle checked={form.autoDeploy} onChange={(v) => setForm({ ...form, autoDeploy: v })} /></Field>
            </> },
            { title: 'webhook', body: <>
              <Field label="endpoint" help="registered on every connected account; GitHub retries for 3 days"><Input readOnly value="https://temps.acme.sh/api/git/webhooks/github" className="h-8 font-mono text-xs" /></Field>
              <Field label="secret" help="rotating re-registers the webhook on all connections; pushes during the swap are replayed">
                <div className="flex items-center gap-2"><Input readOnly value="whsec_••••••••••••" className="h-8 font-mono text-xs" />
                  <EchoDialog trigger={<Button size="sm" variant="outline" className="h-8 text-xs"><RefreshCw /> rotate</Button>} echo={`$ temps git provider rotate-secret ${p0.name}`} title="Rotate webhook secret" description="Generates a new secret and updates every connected account. About 10 seconds." confirmWord={p0.name} steps={['generate secret', 'update 2 webhooks', 'verify delivery']} onDone={() => notify('ok', 'webhook secret rotated', p0.name)} />
                </div>
              </Field>
            </> },
          ]}
          danger={<div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Delete this provider</p><p className="text-[11px] text-muted-foreground">{p0.repos} connected repositories stop deploying on push. Projects keep their last deployment.</p></div>
            <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive"><Trash2 /> delete provider</Button>} echo={`$ temps git provider delete ${p0.name}`} title="Delete provider" description={`Removes ${p0.name} and its ${mine.length} connections. Webhooks are unregistered. Repositories can be reconnected through another provider.`} confirmWord={p0.name} steps={['unregister webhooks', 'detach repositories', 'delete record']} onDone={() => { notify('warn', `${p0.name} deleted`); go('git') }} /></div>} />
      </Detail>
    )
  }

  return (
    <Detail title={p0.name} mark={<GitProviderLogo type={p0.provider_type} className="h-5 w-5" />} meta={`${p0.kind} · ${p0.auth_method === 'app' ? 'installation' : 'personal access token'} · ${p0.base_url} · added ${p0.created_at}`} status={status} tabs={GP_TABS} tab={tab} onTab={setTab}
      actions={<><Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => notify('ok', 'syncing all connections')}><RefreshCw /> sync all</Button><Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'connect account', 'opens GitHub')}><Plus /> connect account <ExternalLink className="opacity-60" /></Button></>}>
      {tab === 'overview' && (
        <div className="space-y-6">
          <MetricGrid cols={4}>
            <Metric label="connections" value={mine.length} baseline={`${mine.filter((c) => c.is_expired).length} expired`} state={expired ? 'error' : 'ok'} />
            <Metric label="repositories" value={mine.reduce((n, c) => n + c.synced_repository_count, 0)} baseline={`${p0.repos - 3} deploying on push`} />
            <Metric label="pushes · 24h" value={42} baseline="38 deployed · 4 ignored (draft branches)" />
            <Metric label="webhook deliveries" value="99.6" unit="%" baseline="1 retry in 7d" state="ok" />
          </MetricGrid>
          {p0.auth_method === 'token' && (
            <p className="text-xs text-muted-foreground">Authenticated with a personal access token, stored encrypted. The token owner's permissions apply and deploy-on-push needs the webhook added by hand; a GitHub App does both itself. <Phrase onClick={() => notify('ok', 'convert to app', 'opens GitHub')}>Convert to GitHub App</Phrase></p>
          )}
          {expired && (
            <Callout state="error" title="acme-org is disconnected: the GitHub App was suspended" quote={expired.health_message}
              action={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={reconnect}>reconnect acme-org</Button>}>
              Pushes to acme-org repositories have not deployed for 2 days; 31 repositories are affected. Reconnect re-authorizes the app on GitHub; nothing else changes.
            </Callout>
          )}
          <Section title="Connections" meta={`${mine.length} · checked every 10 min`} action={<button type="button" className="underline underline-offset-4" onClick={() => setTab('connections')}>all connections</button>}>
            <ul className="op-rows border text-xs">
              {mine.slice(0, 5).map((c) => (
                <li key={c.id} className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                  <span className="min-w-0 truncate font-medium">{c.account_name}</span>
                  <Status state={CONN_STATE[c.health_status]} label={c.is_expired ? 'token expired' : c.health_status} />
                  <span className="ml-auto font-mono text-[11px] text-muted-foreground">{c.synced_repository_count} repos · synced {c.last_synced_at}</span>
                </li>
              ))}
            </ul>
          </Section>
        </div>
      )}
      {tab === 'connections' && (
        <Ledger status={null} columns={[{ label: 'account', key: 'account' }, 'health', 'type', 'installation', { label: 'repos', key: 'repos', numeric: true }, 'synced', '']} grid="1fr 2fr minmax(90px,max-content) minmax(90px,max-content) minmax(55px,max-content) minmax(70px,max-content) minmax(80px,max-content)"
          rows={connRows} total={mine.length} filter={cq} onFilter={setCq} placeholder="filter accounts" dense={dense} footer={<>{connRows.length} of {mine.length} connections · an expired connection keeps its repositories; deploys resume after reconnect</>} />
      )}
    </Detail>
  )
}

// ── Security ───────────────────────────────────────────────────────────

const SCANS = [
  { id: 402, project: 'api-gateway', environment: 'production', scanner_type: 'trivy', scanner_version: '0.58.1', status: 'completed', commit_hash: '9bc61c0', dep: 'dep_91a', critical_count: 2, high_count: 5, medium_count: 14, low_count: 31, completed_at: '41m ago', target: 'ghcr.io/acme/api-gateway:9bc61c0' },
  { id: 401, project: 'api-gateway', environment: 'staging', scanner_type: 'trivy', scanner_version: '0.58.1', status: 'completed', commit_hash: 'd41f9e0', dep: 'dep_93c', critical_count: 0, high_count: 5, medium_count: 12, low_count: 29, completed_at: '18m ago', target: 'ghcr.io/acme/api-gateway:d41f9e0' },
  { id: 398, project: 'acme-storefront', environment: 'production', scanner_type: 'trivy', scanner_version: '0.58.1', status: 'completed', commit_hash: 'c0ffee1', dep: 'dep_88c', critical_count: 0, high_count: 0, medium_count: 3, low_count: 8, completed_at: '3d ago', target: 'ghcr.io/acme/storefront:c0ffee1' },
  { id: 397, project: 'billing-worker', environment: 'production', scanner_type: 'trivy', scanner_version: '0.58.1', status: 'failed', commit_hash: '4f21a8d', dep: 'dep_90e', critical_count: 0, high_count: 0, medium_count: 0, low_count: 0, completed_at: '2h ago', target: 'ghcr.io/acme/billing-worker:4f21a8d', error: 'trivy: image pull timed out after 120s (registry.acme.sh)' },
  { id: 390, project: 'docs', environment: 'production', scanner_type: 'trivy', scanner_version: '0.57.0', status: 'completed', commit_hash: '7a11c3e', dep: 'dep_71b', critical_count: 0, high_count: 0, medium_count: 0, low_count: 2, completed_at: '12d ago', target: 'ghcr.io/acme/docs:7a11c3e' },
]
const VULNS = [
  { vulnerability_id: 'CVE-2026-31841', severity: 'CRITICAL', cvss_score: 9.8, package_name: 'openssl', installed_version: '3.3.1-r0', fixed_version: '3.3.2-r0', title: 'Buffer overflow in X.509 name constraint checking', target: 'alpine 3.20' },
  { vulnerability_id: 'CVE-2026-29017', severity: 'CRITICAL', cvss_score: 9.1, package_name: 'undici', installed_version: '6.19.2', fixed_version: '6.21.1', title: 'Proxy-Authorization header leaked on cross-origin redirect', target: 'node_modules' },
  { vulnerability_id: 'CVE-2026-27780', severity: 'HIGH', cvss_score: 7.5, package_name: 'busybox', installed_version: '1.36.1-r29', fixed_version: '1.36.1-r31', title: 'awk: out-of-bounds read in getvar_s', target: 'alpine 3.20' },
  { vulnerability_id: 'CVE-2026-26301', severity: 'HIGH', cvss_score: 7.5, package_name: 'path-to-regexp', installed_version: '6.2.1', fixed_version: '6.3.0', title: 'ReDoS with two parameters in one segment', target: 'node_modules' },
  { vulnerability_id: 'CVE-2026-25566', severity: 'HIGH', cvss_score: 7.3, package_name: 'libcurl', installed_version: '8.9.0-r0', fixed_version: '8.10.0-r0', title: 'ASN.1 date parser overread', target: 'alpine 3.20' },
  { vulnerability_id: 'CVE-2026-24122', severity: 'MEDIUM', cvss_score: 5.3, package_name: 'micromatch', installed_version: '4.0.5', fixed_version: '4.0.8', title: 'Inefficient regular expression complexity', target: 'node_modules' },
]
const SEV_STATE: Record<string, State> = { CRITICAL: 'error', HIGH: 'error', MEDIUM: 'warn', LOW: 'idle' }
const scanState = (s: (typeof SCANS)[number]): State => (s.status === 'failed' ? 'error' : s.critical_count > 0 ? 'error' : s.high_count > 0 ? 'warn' : 'ok')
const SEC_TABS = ['scans', 'headers', 'access'] as const

export function SecurityScreen({ dense, notify, go }: { dense: boolean; notify: Notify; go: (v: string) => void }) {
  const [tab, setTab] = useState<(typeof SEC_TABS)[number]>('scans')
  const [q, setQ] = useState('')
  const worst = SCANS.find((s) => s.critical_count > 0)
  const failedScan = SCANS.find((s) => s.status === 'failed')
  const items: StatusItem[] = []
  if (failedScan) items.push({ state: 'error', children: <>Scan of {failedScan.project} {failedScan.environment} failed: image pull timed out. <Phrase onClick={() => notify('ok', 'rescan queued', failedScan.project)}>Rescan</Phrase></> })
  const status = (
    <StatusLine state={worst || failedScan ? 'error' : 'ok'} more={items.length ? { label: `+${items.length} more`, items } : undefined}>
      {worst ? <><Phrase onClick={() => go(`scan:${worst.id}`)}>{worst.critical_count} critical</Phrase> in {worst.project} {worst.environment} since {worst.dep}.</> : <>No critical findings in any environment.</>}
    </StatusLine>
  )
  const rows: LedgerRow[] = SCANS.filter((s) => matches(q, s.project, s.environment, s.dep, s.commit_hash)).map((s) => ({
    id: String(s.id), state: scanState(s), onOpen: () => go(`scan:${s.id}`),
    sort: { project: s.project, critical: s.critical_count, high: s.high_count, medium: s.medium_count, low: s.low_count, when: s.id },
    mobile: <><span className="block truncate font-medium">{s.project} · {s.environment}</span><span className="block truncate text-[11px] text-muted-foreground">{s.status === 'failed' ? 'scan failed' : `${s.critical_count} critical · ${s.high_count} high · ${s.dep}`}</span></>,
    cells: [
      <span className="font-medium">{s.project} <span className="font-normal text-muted-foreground">{s.environment}</span></span>,
      <Status state={scanState(s)} label={s.status === 'failed' ? 'scan failed · image pull timeout' : s.critical_count ? 'critical findings' : s.high_count ? 'high findings' : 'clean'} />,
      <span className="font-mono text-muted-foreground">{s.dep} · {s.commit_hash}</span>,
      <Num value={s.status === 'failed' ? null : s.critical_count} className={cn(s.critical_count > 0 && 'text-destructive')} />,
      <Num value={s.status === 'failed' ? null : s.high_count} className={cn(s.high_count > 0 && 'text-warning')} />,
      <Num value={s.status === 'failed' ? null : s.medium_count} />,
      <Num value={s.status === 'failed' ? null : s.low_count} />,
      <span className="text-muted-foreground">{s.completed_at}</span>,
    ],
  }))

  const [headers, setHeaders] = useState({ enabled: true, preset: 'strict', hsts: 'max-age=63072000; includeSubDomains; preload', xfo: 'DENY', referrer: 'strict-origin-when-cross-origin', xcto: 'nosniff', csp: "default-src 'self'; img-src 'self' data: https:; script-src 'self' https://temps.acme.sh; connect-src 'self' https://temps.acme.sh", permissions: 'camera=(), microphone=(), geolocation=()' })
  const [savedH, setSavedH] = useState(headers)
  const [access, setAccess] = useState({ password: false, rate: true, rps: '120', geo: false, countries: '', attack: 'off', allow: '203.0.113.0/24' })
  const [savedA, setSavedA] = useState(access)

  return (
    <Detail title="Security" meta={`${SCANS.length} environments scanned · trivy 0.58.1 · headers preset ${headers.preset}`} status={status} tabs={SEC_TABS} tab={tab} onTab={setTab}
      actions={tab === 'scans' ? <Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'rescanning 5 environments', 'runs after the next deploy anyway')}><RefreshCw /> rescan all</Button> : undefined}>
      {tab === 'scans' && (
        <Ledger status={null} columns={[{ label: 'environment', key: 'project' }, 'result', 'image', { label: 'critical', key: 'critical', numeric: true }, { label: 'high', key: 'high', numeric: true }, { label: 'medium', key: 'medium', numeric: true }, { label: 'low', key: 'low', numeric: true }, { label: 'scanned', key: 'when' }]} grid="1.5fr 1.8fr minmax(120px,max-content) minmax(50px,max-content) minmax(50px,max-content) minmax(50px,max-content) minmax(50px,max-content) minmax(70px,max-content)"
          rows={rows} total={SCANS.length} filter={q} onFilter={setQ} placeholder="filter by project or environment" hint="critical first" dense={dense}
          footer={<>{rows.length} of {SCANS.length} · every deploy is scanned after it goes live; a critical finding does not block the deploy, it shows here and on the project</>} />
      )}
      {tab === 'headers' && (
        <Settings status={null} dirty={JSON.stringify(headers) !== JSON.stringify(savedH)} onSave={() => { setSavedH(headers); notify('ok', 'security headers saved', 'applied to all routes on the next request') }}
          sections={[
            { title: 'preset', body: <>
              <Field label="send security headers" help="added by the proxy to every response; per-project settings can override"><Toggle checked={headers.enabled} onChange={(v) => setHeaders({ ...headers, enabled: v })} /></Field>
              <Field label="preset" help="strict is the default for new installs; custom keeps whatever you type below"><Picker value={headers.preset} onChange={(v) => setHeaders({ ...headers, preset: v })} options={[{ value: 'strict', meta: 'HSTS preload · DENY · nosniff · CSP self' }, { value: 'balanced', meta: 'HSTS · SAMEORIGIN · no CSP' }, { value: 'custom', meta: 'your values below' }]} /></Field>
            </> },
            { title: 'headers', body: <>
              <Field label="Strict-Transport-Security"><Input value={headers.hsts} onChange={(e) => setHeaders({ ...headers, hsts: e.target.value, preset: 'custom' })} className="h-8 font-mono text-xs" /></Field>
              <Field label="X-Frame-Options"><Picker value={headers.xfo} onChange={(v) => setHeaders({ ...headers, xfo: v, preset: 'custom' })} options={[{ value: 'DENY' }, { value: 'SAMEORIGIN' }, { value: 'off', label: 'not sent' }]} /></Field>
              <Field label="Referrer-Policy"><Picker value={headers.referrer} onChange={(v) => setHeaders({ ...headers, referrer: v, preset: 'custom' })} options={['no-referrer', 'strict-origin', 'strict-origin-when-cross-origin', 'same-origin'].map((v) => ({ value: v }))} /></Field>
              <Field label="X-Content-Type-Options"><Input value={headers.xcto} onChange={(e) => setHeaders({ ...headers, xcto: e.target.value, preset: 'custom' })} className="h-8 font-mono text-xs" /></Field>
              <Field label="Content-Security-Policy" help="one line; the console warns if a connected domain is not allowed by connect-src"><Input value={headers.csp} onChange={(e) => setHeaders({ ...headers, csp: e.target.value, preset: 'custom' })} className="h-8 font-mono text-xs" /></Field>
              <Field label="Permissions-Policy"><Input value={headers.permissions} onChange={(e) => setHeaders({ ...headers, permissions: e.target.value, preset: 'custom' })} className="h-8 font-mono text-xs" /></Field>
            </> },
          ]}
          danger={<div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Reset to strict preset</p><p className="text-[11px] text-muted-foreground">Discards custom values on all projects that do not override.</p></div>
            <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive">reset</Button>} echo="$ temps security headers reset --preset strict" title="Reset security headers" description="Replaces the header values above with the strict preset." confirmWord="strict" steps={['write preset', 'reload proxy config']} onDone={() => { const s = { ...headers, preset: 'strict' }; setHeaders(s); setSavedH(s); notify('ok', 'headers reset to strict') }} /></div>} />
      )}
      {tab === 'access' && (
        <Settings status={null} dirty={JSON.stringify(access) !== JSON.stringify(savedA)} onSave={() => { setSavedA(access); notify('ok', 'access rules saved') }}
          sections={[
            { title: 'rate limiting', body: <>
              <Field label="rate limit" help={access.rate ? 'per client IP, across all routes; 429 with Retry-After' : 'off · every client can send as many requests as the upstream takes'}><Toggle checked={access.rate} onChange={(v) => setAccess({ ...access, rate: v })} /></Field>
              <Field label="requests / second" help={access.rate ? 'the proxy counts in memory; nothing is written per request' : 'turn rate limiting on to set a limit'}><Input value={access.rps} disabled={!access.rate} onChange={(e) => setAccess({ ...access, rps: e.target.value })} className="h-8 w-28 font-mono text-xs" /></Field>
            </> },
            { title: 'challenge', body: <>
              <Field label="attack mode" help="off · challenge (a captcha page for new clients) · block. Turn on during an incident, turn off after; it is not meant to stay on"><Picker value={access.attack} onChange={(v) => setAccess({ ...access, attack: v })} mono={false} options={[{ value: 'off' }, { value: 'challenge', meta: 'captcha, 24h cookie' }, { value: 'block', meta: '403 except allow-list' }]} /></Field>
              <Field label="allow-list" help="CIDRs that skip rate limits and challenges"><Input value={access.allow} onChange={(e) => setAccess({ ...access, allow: e.target.value })} className="h-8 font-mono text-xs" /></Field>
            </> },
            { title: 'restrictions', body: <>
              <Field label="password protection" help="a shared password page in front of every environment that opts in; previews use it by default"><Toggle checked={access.password} onChange={(v) => setAccess({ ...access, password: v })} /></Field>
              <Field label="geo restrictions" help="block or allow by country; needs the GeoIP database, which is present"><Toggle checked={access.geo} onChange={(v) => setAccess({ ...access, geo: v })} /></Field>
              <Field label="countries" help="ISO codes, comma separated"><Input value={access.countries} onChange={(e) => setAccess({ ...access, countries: e.target.value })} placeholder="e.g. RU, KP" disabled={!access.geo} className="h-8 font-mono text-xs" /></Field>
            </> },
          ]}
          danger={<div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Block all traffic</p><p className="text-[11px] text-muted-foreground">Every request gets a 403 except the allow-list. Deploys and the console keep working.</p></div>
            <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive">block all</Button>} echo="$ temps security attack-mode block" title="Block all traffic" description="Sets attack mode to block on every environment until you turn it off. The allow-list still gets through." confirmWord="block" steps={['write config', 'reload proxy']} onDone={() => { const a = { ...access, attack: 'block' }; setAccess(a); setSavedA(a); notify('warn', 'attack mode: block', 'all environments') }} /></div>} />
      )}
    </Detail>
  )
}

export function ScanScreen({ id, dense, notify }: { id: string; dense: boolean; /** The trail back to the scans list lives in the shell header, not on the page. */ go?: (v: string) => void; notify: Notify }) {
  const s = SCANS.find((x) => String(x.id) === id) ?? SCANS[0]
  const [q, setQ] = useState('')
  const [only, setOnly] = useState<'all' | 'fixable'>('all')
  const list = VULNS.filter((v) => matches(q, v.package_name, v.vulnerability_id, v.title, v.target) && (only === 'all' || v.fixed_version))
  const rows: LedgerRow[] = list.map((v) => ({
    id: v.vulnerability_id, state: SEV_STATE[v.severity], onOpen: () => notify('ok', `open ${v.vulnerability_id}`),
    sort: { cve: v.vulnerability_id, cvss: v.cvss_score, pkg: v.package_name },
    mobile: <><span className="block truncate font-medium">{v.vulnerability_id} · {v.package_name}</span><span className="block truncate text-[11px] text-muted-foreground">{v.severity.toLowerCase()} {v.cvss_score} · fix {v.fixed_version ?? 'none'}</span></>,
    cells: [
      <span className="font-mono">{v.vulnerability_id}</span>,
      <Status state={SEV_STATE[v.severity]} label={v.severity.toLowerCase()} />,
      <Num value={v.cvss_score} />,
      <span className="font-medium">{v.package_name}</span>,
      <span className="font-mono text-muted-foreground">{v.installed_version} → {v.fixed_version ?? <Status state="warn" label="no fix" />}</span>,
      <span className="truncate">{v.title}</span>,
      <span className="text-muted-foreground">{v.target}</span>,
    ],
  }))
  const fixable = VULNS.filter((v) => v.fixed_version).length
  return (
    <Detail title={<span>{s.project} <span className="text-muted-foreground">{s.environment}</span></span>} meta={`scan #${s.id} · ${s.dep} · ${s.commit_hash}`}
      status={<StatusLine state={scanState(s)}>Rebuild to pick up <Phrase onClick={() => setQ('openssl')}>openssl 3.3.2</Phrase>: it closes the 9.8 in this image.</StatusLine>}
      lede={
        <Lede state={scanState(s)} word={`${s.critical_count} critical`} facts={[
          { k: 'critical', v: String(s.critical_count), state: s.critical_count ? 'error' : undefined },
          { k: 'high', v: String(s.high_count), state: s.high_count ? 'warn' : undefined },
          { k: 'medium · low', v: `${s.medium_count} · ${s.low_count}` },
          { k: 'fixable', v: `${fixable} of ${VULNS.length}` },
          { k: 'image', v: s.target },
          { k: 'scanned', v: s.completed_at },
        ]}>
          A rebuild updates the base image and the lockfile; a critical finding does not block the deploy.
        </Lede>
      }
      actions={<Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'rebuild queued', `${s.project} · ${s.environment}`)}><RefreshCw /> rebuild and rescan</Button>}>
      <Columns>
        <div>
          <Section title="Findings" meta={`${VULNS.length} listed`}>
            <Ledger status={null} columns={[{ label: 'cve', key: 'cve' }, 'severity', { label: 'cvss', key: 'cvss', numeric: true }, { label: 'package', key: 'pkg' }, 'installed → fixed', 'title', 'where']} grid="minmax(120px,max-content) minmax(80px,max-content) minmax(50px,max-content) minmax(90px,max-content) minmax(140px,max-content) 2fr minmax(90px,max-content)"
              rows={rows} total={VULNS.length} filter={q} onFilter={setQ} placeholder="filter by CVE or package" hint="severity, then cvss" dense={dense}
              action={<Segmented options={[['all', 'all'], ['fixable', 'fixable']] as const} value={only} onChange={setOnly} />}
              footer={<>{rows.length} of {VULNS.length} shown (6 of {s.critical_count + s.high_count + s.medium_count + s.low_count} in this mockup) · findings link to the advisory · rebuild is the fix for all six</>} />
          </Section>
        </div>
        <div>
          <Section title="Image" meta="4">
            <KeyValue compact rows={[
              { k: 'digest', v: 'sha256:8f3a…c91e' },
              { k: 'base', v: 'node:22-alpine3.20' },
              { k: 'layers', v: '11 · 184 MB' },
              { k: 'scanner', v: `${s.scanner_type} ${s.scanner_version} · db 2026-09-04` },
            ]} />
          </Section>
        </div>
      </Columns>
    </Detail>
  )
}

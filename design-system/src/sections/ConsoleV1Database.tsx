// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type CSSProperties } from 'react'
import { ArrowUpRight, Database, HardDrive, Link, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import {
  ChartFooter, Detail, EchoDialog, Ledger, Lede, Metric, MetricGrid, Num, PageState, Phrase, Section, Segmented, KeyValue, Status, StatusLine, TimeChart, Columns, SecretValue, ProjectMark,
  StatusStrip, LogLines, Histogram, quantile,
  type KV, type LedgerRow, type State, type StatusBucket, type LogLine, type Pct, type HistBucket,
} from '@/components/op'
import type { Notify } from './ConsoleV1Observe'
import { PROJECT_ICONS } from './console-projects'
import { agoNum, sizeNum } from './ConsoleV1'

/**
 * A managed database is one record with a lot attached to it. The rule is
 * the record recipe: the verdict says the one thing to act on, the lede
 * holds the five facts, the content column is health then backups then
 * alerts, the aside is how to reach it and what it runs on. Everything
 * that is a list or a tool (backups, all metrics, logs, slow queries, the
 * data browser) is a facet with its own tab, never a card stacked under
 * the overview.
 */

// ── Data ─────────────────────────────────────────────────────────────
type Db = { id: string; name: string; engine: string; version: string; env: string; node: string; created: string; image: string; port: number; host: string; password: string; volume: string; memLimit: string; linked: string[]; backups: Backup[]; pitr: boolean; lastCheck: string }
type Backup = { id: string; at: string; size: string; source: string; state: State; note?: string }
const DBS: Db[] = [
  { id: 'sessions-redis', name: 'sessions-redis', engine: 'Redis', version: '7.2', env: 'production', node: 'hetzner-1', created: '5 minutes ago', image: 'gotempsh/redis-walg:7-bookworm', port: 6385, host: 'localhost', password: 'r3d1s-s3cr3t-k9x', volume: '/var/lib/temps/sessions-redis', memLimit: '256 MB', linked: [], backups: [], pitr: false, lastCheck: '40s ago' },
  { id: 'acme-pg', name: 'acme-pg', engine: 'PostgreSQL', version: '18.1', env: 'production', node: 'hetzner-1', created: '4 months ago', image: 'timescale/timescaledb-ha:pg18', port: 5433, host: 'localhost', password: 'pg-x7Qm-4t1s-Zz9v', volume: '/var/lib/temps/acme-pg', memLimit: '2 GB', linked: ['acme-storefront', 'api-gateway'], pitr: true, lastCheck: '12s ago', backups: [
    { id: 'b_41', at: '2h ago', size: '4.2 GB', source: 'r2-backups', state: 'ok' }, { id: 'b_40', at: '1d ago', size: '4.1 GB', source: 'r2-backups', state: 'ok' }, { id: 'b_39', at: '2d ago', size: '4.1 GB', source: 'r2-backups', state: 'ok' },
    { id: 'b_38', at: '3d ago', size: '4.0 GB', source: 'r2-backups', state: 'error', note: 'upload timed out after 3 parts' }, { id: 'b_37', at: '4d ago', size: '4.0 GB', source: 'r2-backups', state: 'ok' }, { id: 'b_36', at: '5d ago', size: '3.9 GB', source: 'r2-backups', state: 'ok' },
  ] },
  { id: 'events-ch', name: 'events-ch', engine: 'ClickHouse', version: '24.8', env: 'production', node: 'hetzner-2', created: '2 months ago', image: 'clickhouse/clickhouse-server:24.8', port: 9000, host: 'localhost', password: 'ch-m2Pq-8vLk-Rr3t', volume: '/var/lib/temps/events-ch', memLimit: '4 GB', linked: ['acme-storefront'], pitr: false, lastCheck: '20s ago', backups: [{ id: 'b_12', at: '3d ago', size: '38 GB', source: 'r2-backups', state: 'ok' }] },
]
type MetricDef = { key: string; label: string; unit: string; fmt: (v: number) => string; state?: State; series: number[] }
const series = (base: number, amp: number, seed: number) => Array.from({ length: 48 }, (_, i) => +(base + Math.abs(Math.sin((i + seed) / 4.2)) * amp).toFixed(2))
const METRICS: Record<string, MetricDef[]> = {
  Redis: [
    { key: 'mem', label: 'memory used', unit: 'MB', fmt: (v) => `${v.toFixed(2)} MB`, series: series(1.1, 0.3, 1) },
    { key: 'clients', label: 'clients', unit: '', fmt: (v) => String(Math.round(v)), series: series(1, 0, 0) },
    { key: 'hit', label: 'hit ratio', unit: '%', fmt: (v) => (v ? `${v.toFixed(1)}%` : '—'), series: series(0, 0, 0), state: 'idle' },
    { key: 'evicted', label: 'evicted keys', unit: '', fmt: (v) => String(Math.round(v)), series: series(0, 0, 0) },
    { key: 'cpu', label: 'cpu', unit: '%', fmt: (v) => `${v.toFixed(1)}%`, series: series(0.8, 0.6, 3) },
  ],
  PostgreSQL: [
    { key: 'conn', label: 'connections', unit: '', fmt: (v) => `${Math.round(v)} / 100`, series: series(34, 22, 2) },
    { key: 'tps', label: 'transactions', unit: '/s', fmt: (v) => `${Math.round(v)}/s`, series: series(210, 160, 5) },
    { key: 'cache', label: 'cache hit', unit: '%', fmt: (v) => `${v.toFixed(1)}%`, series: series(98.2, 1.2, 1) },
    { key: 'size', label: 'size', unit: 'GB', fmt: (v) => `${v.toFixed(2)} GB`, series: series(4.1, 0.1, 0) },
    { key: 'cpu', label: 'cpu', unit: '%', fmt: (v) => `${v.toFixed(1)}%`, series: series(12, 18, 3), state: 'ok' },
  ],
  ClickHouse: [
    { key: 'qps', label: 'queries', unit: '/s', fmt: (v) => `${Math.round(v)}/s`, series: series(40, 30, 4) },
    { key: 'mem', label: 'memory used', unit: 'GB', fmt: (v) => `${v.toFixed(2)} GB`, series: series(3.1, 0.7, 2), state: 'warn' },
    { key: 'parts', label: 'active parts', unit: '', fmt: (v) => String(Math.round(v)), series: series(120, 40, 1) },
    { key: 'size', label: 'size', unit: 'GB', fmt: (v) => `${v.toFixed(1)} GB`, series: series(38, 0.4, 0) },
    { key: 'cpu', label: 'cpu', unit: '%', fmt: (v) => `${v.toFixed(1)}%`, series: series(30, 25, 3) },
  ],
}
const uptime = (flaky: boolean): StatusBucket[] => Array.from({ length: 48 }, (_, i) => { const down = flaky && i >= 18 && i <= 20; return { start: `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`, state: down ? 'error' : 'ok', checks: 60, down: down ? 60 : 0, p50_ms: down ? undefined : 2, p95_ms: down ? undefined : 4 } })
const ALERTS = [{ name: 'memory above 80% of limit', state: 'ok' as State, note: 'now 0.5%' }, { name: 'clients above 100', state: 'ok' as State, note: 'now 1' }]
const LOGS: LogLine[] = Array.from({ length: 40 }, (_, i) => ({ t: `10:${String(15 + Math.floor(i / 4)).padStart(2, '0')}:${String((i * 13) % 60).padStart(2, '0')}`, level: i === 22 ? 'warn' : 'info', source: 'redis', msg: i === 22 ? 'Client closed connection during MULTI' : i % 9 === 0 ? 'DB saved on disk' : i % 5 === 0 ? 'Background saving started by pid 71' : 'Accepted 172.18.0.4:51322' }))
const SLOW = [
  { q: 'SELECT * FROM orders WHERE customer_id = $1 ORDER BY created_at DESC', calls: 12_400, mean: 184, total: 38.1, note: 'no index on customer_id' }, { q: 'UPDATE sessions SET last_seen = now() WHERE id = $1', calls: 91_000, mean: 3.1, total: 4.7 },
  { q: 'SELECT count(*) FROM events WHERE project_id = $1 AND ts > $2', calls: 2_100, mean: 640, total: 22.4, note: 'seq scan · 38 GB' }, { q: 'INSERT INTO audit_logs (...) VALUES (...)', calls: 44_000, mean: 1.2, total: 0.9 },
]
const HIST: HistBucket[] = [[1, 820], [5, 4100], [20, 2200], [100, 900], [500, 310], [2000, 40]].map(([le, count]) => ({ le, count }))

// ── Screen ───────────────────────────────────────────────────────────
const TABS = ['overview', 'backups', 'metrics', 'logs', 'queries', 'data'] as const
type Tab = (typeof TABS)[number]
export function DatabaseScreen({ id, dense, notify, go }: { id: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const db = DBS.find((d) => d.id === id) ?? DBS[0]
  const metrics = METRICS[db.engine]
  const [tab, setTab] = useState<Tab>('overview')
  const [metric, setMetric] = useState(metrics[0].key)
  const [range, setRange] = useState<'1h' | '6h' | '24h' | '7d'>('1h')
  const [reveal, setReveal] = useState(false)
  const [q, setQ] = useState('')
  const [pct, setPct] = useState<Pct>('p95')
  const [live, setLive] = useState(true)
  const [dataView, setDataView] = useState<'tree' | 'sql'>('tree')
  const [statement, setStatement] = useState('')
  const [ran, setRan] = useState<string | null>(null)
  const m = metrics.find((x) => x.key === metric) ?? metrics[0]
  const last = (d: MetricDef) => d.series[d.series.length - 1]
  const noBackup = db.backups.length === 0
  const failed = db.backups.find((b) => b.state === 'error')
  const UPTIME = uptime(db.id === 'sessions-redis')
  const downtime = UPTIME.filter((b) => b.state === 'error').length * 30
  const url = `${db.engine === 'Redis' ? 'redis' : db.engine === 'PostgreSQL' ? 'postgres' : 'clickhouse'}://default:${reveal ? db.password : '••••••••'}@${db.host}:${db.port}`

  const status = noBackup
    ? <StatusLine state="warn" more={{ label: '+1', items: [{ state: 'warn', children: <>Down for {downtime} min yesterday at 09:00: the container restarted after the node ran out of memory. <Phrase onClick={() => go('node:hetzner-1')}>hetzner-1</Phrase> is at 91%.</> }] }}>No backup has ever been taken. If this volume is lost there is nothing to restore from. <Phrase onClick={() => setTab('backups')}>Take one now</Phrase> or schedule it from an S3 source.</StatusLine>
    : failed
      ? <StatusLine state="warn">Backup {failed.id} failed {failed.at}: {failed.note}. The last good one was taken {db.backups[0].at}.</StatusLine>
      : /\dd/.test(db.backups[0].at)
        ? <StatusLine state="warn">The nightly backup has not run since {db.backups[0].at}. <Phrase onClick={() => go('backups')}>Check the schedule and the S3 source</Phrase>; until then a restore loses everything after {db.backups[0].at}.</StatusLine>
      : <StatusLine state="ok">Healthy. Last backup {db.backups[0]?.at}{db.pitr ? ', point-in-time recovery to any second in the last 7 days' : ''}.</StatusLine>

  const facts: KV[] = [
    { k: 'uptime 24h', v: `${(100 - (downtime / 1440) * 100).toFixed(2)}%`, mono: true, state: downtime ? 'warn' : undefined },
    { k: 'response', v: '2 ms', mono: true },
    { k: metrics[0].label, v: metrics[0].fmt(last(metrics[0])) + (metrics[0].key === 'mem' ? ` of ${db.memLimit}` : ''), mono: true },
    { k: metrics[1].label, v: metrics[1].fmt(last(metrics[1])), mono: true },
    { k: 'last backup', v: noBackup ? 'never' : db.backups[0].at, mono: true, state: noBackup ? 'warn' : undefined },
    // Marks only: a row of favicons reads as "who depends on this" at a glance; the name is on hover and in the aside list.
    { k: 'linked projects', v: db.linked.length ? <span className="inline-flex items-center gap-1" aria-label={db.linked.join(', ')}>{db.linked.map((p) => (
      <Tooltip key={p} delayDuration={0}><TooltipTrigger asChild><button type="button" onClick={() => go(p)} aria-label={p} className="inline-flex rounded-none outline-none hover:opacity-80 focus-visible:ring-1 focus-visible:ring-foreground"><ProjectMark name={p} icon={PROJECT_ICONS[p]} /></button></TooltipTrigger><TooltipContent side="bottom" className="font-mono text-[11px]">{p}</TooltipContent></Tooltip>
    ))}</span> : 'none', mono: true },
  ]
  const lede = <Lede state={downtime ? 'warn' : 'ok'} word="running" facts={facts}>checked {db.lastCheck}{downtime ? ' · restarted once in the last 24h' : ''}</Lede>

  const chart = (height: number) => (
    <div className="space-y-2">
      <div className="border bg-background p-3">
        <TimeChart data={m.series.map((v, i) => ({ t: `${String(9 + Math.floor(i / 4)).padStart(2, '0')}:${String((i % 4) * 15).padStart(2, '0')}`, v }))} series={[{ key: 'v', name: m.label }]} unit={m.unit} height={height} xInterval={7} readoutFormat={(p) => `${p.t} · ${m.label} ${m.fmt(Number(p.v))}`} />
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2"><ChartFooter><span>{m.label} · {range}</span>{m.state === 'idle' && <span>· ○ no reads yet, so no hit ratio</span>}</ChartFooter><Segmented options={[['1h', '1h'], ['6h', '6h'], ['24h', '24h'], ['7d', '7d']] as const} value={range} onChange={setRange} className="h-6 [&>button]:h-6" /></div>
    </div>
  )
  const metricStrip = (defs: MetricDef[]) => (
    <div className="op-tiles" style={{ '--tiles': 5 } as CSSProperties}>
      {defs.map((d) => { const on = d.key === metric; return (
        <button key={d.key} type="button" aria-pressed={on} onClick={() => setMetric(d.key)} className={`min-w-0 p-3 text-left transition-colors hover:bg-muted/40 ${on ? 'bg-muted/60' : ''}`}>
          <p className="op-label truncate">{d.label}</p>
          <p className="mt-1 flex items-baseline gap-2 font-mono text-lg leading-6">{d.fmt(last(d))}{d.state && d.state !== 'ok' && <span className="text-xs"><Status state={d.state} label={d.state === 'idle' ? 'no reads' : d.state === 'warn' ? 'near limit' : ''} /></span>}</p>
        </button>
      ) })}
    </div>
  )

  const bq = q.trim().toLowerCase()
  const backupRows: LedgerRow[] = db.backups.filter((b) => b.id.toLowerCase().includes(bq) || b.source.toLowerCase().includes(bq)).map((b) => ({
    id: b.id, state: b.state, onOpen: () => notify('ok', `open ${b.id}`, 'run detail: steps, size per part, restore'),
    sort: { id: b.id, size: sizeNum(b.size), at: agoNum(b.at) },
    mobile: <><span className="block font-mono">{b.id}</span><span className="block text-[11px] text-muted-foreground">{b.note ?? `${b.size} · ${b.at}`}</span></>,
    cells: [<span className="font-mono">{b.id}</span>, <Status state={b.state} label={b.note ?? (b.state === 'ok' ? 'complete' : '')} />, <Num value={b.size} />, <span className="text-muted-foreground">{b.source}</span>, <span className="text-muted-foreground">{b.at}</span>, <button type="button" className="text-xs text-muted-foreground hover:text-foreground" onClick={(e) => { e.stopPropagation(); notify('ok', `restore from ${b.id}`, 'in place, as a new service, or to a point in time') }}>restore</button>],
  }))
  const slowRows: LedgerRow[] = SLOW.filter((s) => s.q.toLowerCase().includes(q.toLowerCase())).map((s, i) => ({
    id: String(i), state: s.mean > 500 ? 'error' : s.mean > 100 ? 'warn' : 'ok', onOpen: () => notify('ok', 'query detail', 'plan, first seen, callers'),
    sort: { calls: s.calls, mean: s.mean, total: s.total },
    mobile: <><span className="block truncate font-mono">{s.q}</span><span className="block text-[11px] text-muted-foreground">{s.note ?? `${s.mean} ms mean`}</span></>,
    cells: [<span className="truncate font-mono">{s.q}</span>, <Num value={s.calls} />, <Num value={s.mean} unit="ms" />, <Num value={s.total} unit="%" />, <span className="text-muted-foreground">{s.note ?? ''}</span>],
  }))

  return (
    <Detail title={db.name} mark={<span className="inline-flex h-6 w-6 items-center justify-center border text-muted-foreground [&_svg]:h-3.5 [&_svg]:w-3.5"><Database /></span>}
      meta={`${db.engine} ${db.version} · ${db.env} · created ${db.created}`} status={status} lede={tab === 'overview' ? lede : undefined}
      tabs={TABS} tab={tab} onTab={(t) => { setTab(t); setQ('') }}
      actions={<>
        <CopyButton value={url.replace('••••••••', db.password)} label="copy URL" variant="outline" className="h-7 text-xs">copy URL</CopyButton>
        <Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'backup started', `${db.name} → r2-backups`)}><HardDrive /> back up now</Button>
      </>}>

      {tab === 'overview' && (
        <Columns>
          <div>
            <Section title="Health" meta={`checked every 30s · ${downtime ? `${downtime} min down in 24h` : 'no downtime in 24h'}`} action={<a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'checking now', 'tcp connect + PING · 2 ms') }} className="text-xs">check now</a>}>
              <div className="space-y-4">
                <div><StatusStrip buckets={UPTIME} /><p className="mt-1 font-mono text-[11px] text-muted-foreground">last 24h · 30 min per segment · ← → reads a segment</p></div>
                {metricStrip(metrics)}
                {chart(160)}
              </div>
            </Section>
            <Section title="Backup" meta={noBackup ? 'none' : `${db.backups.length} kept · last ${db.backups[0].at}`} action={<a href="#" onClick={(e) => { e.preventDefault(); setTab('backups') }} className="text-xs">all backups</a>}>
              {noBackup ? (
                <div className="border bg-background px-4 py-4 text-xs">
                  <p className="font-medium">No backup has ever been taken.</p>
                  <p className="mt-1 text-muted-foreground">A backup is a compressed snapshot of the volume uploaded to one of your S3 sources; a schedule takes one every night and keeps the last 14. Without one, losing the node loses {db.name}.</p>
                  <div className="mt-3 flex flex-wrap gap-2">
                    <Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'backup started', `${db.name} → r2-backups · b_42`)}><HardDrive /> back up now</Button>
                    <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => go('backups')}>schedule from an S3 source</Button>
                  </div>
                </div>
              ) : (() => {
                // One line answers "am I covered": the last backup, whether it worked, where it is, when the next one runs.
                const last = db.backups[0]
                const failedRecent = db.backups.slice(0, 7).filter((b) => b.state === 'error')
                const daysOld = /(\d+)d/.test(last.at) ? Number(/(\d+)d/.exec(last.at)![1]) : 0
                const missed = daysOld >= 1 ? daysOld : 0 // nightly schedule: anything older than a day is a missed run
                const label = last.state !== 'ok' ? `last backup failed: ${last.note}` : missed ? `last backup ${last.at} · nightly schedule missed ${missed} time${missed === 1 ? '' : 's'}` : 'last backup complete'
                return (
                  <div className="border bg-background px-4 py-3 text-xs">
                    <p className="flex flex-wrap items-baseline gap-x-2"><Status state={last.state !== 'ok' ? last.state : missed ? 'warn' : 'ok'} label={label} /><span className="font-mono text-muted-foreground">{last.id} · {last.size} · to {last.source}{missed ? '' : ` · ${last.at}`}</span></p>
                    <p className="mt-1 text-muted-foreground">{missed ? <>the 02:00 run has not produced a backup since {last.at}: <Phrase onClick={() => go('backups')}>check the schedule and the S3 source</Phrase></> : 'next tonight at 02:00'} · nightly · keeps 14{db.pitr ? ' · point-in-time recovery to any second in the last 7 days' : ''}{failedRecent.length ? ` · ${failedRecent.length} of the last ${Math.min(db.backups.length, 7)} failed (${failedRecent.map((b) => b.id).join(', ')})` : ' · the last 7 all completed'}</p>
                    <div className="mt-3 flex flex-wrap gap-2">
                      <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', `restore from ${last.id}`, 'in place, as a new service, or to a point in time')}>restore from {last.id}</Button>
                      <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'backup started', `${db.name} → ${last.source} · b_42`)}><HardDrive /> back up now</Button>
                    </div>
                  </div>
                )
              })()}
            </Section>
            <Section title="Alert rules" meta={`${ALERTS.length} · none firing`} action={<a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'new alert rule', 'metric · threshold · for · notify') }} className="text-xs">add rule</a>}>
              <ol className="op-rows border bg-background text-xs">
                {ALERTS.map((a) => <li key={a.name} className="flex items-center justify-between gap-3 px-3 py-2"><Status state={a.state} label={a.name} /><span className="font-mono text-muted-foreground">{a.note}</span></li>)}
              </ol>
            </Section>
          </div>
          <div>
            <Section title="Connect" meta="from inside the network" action={<button type="button" className="text-xs text-muted-foreground hover:text-foreground" onClick={() => setReveal((r) => !r)}>{reveal ? 'hide' : 'reveal'}</button>}>
              <KeyValue compact rows={[
                { k: 'host', v: db.host, mono: true, copy: db.host }, { k: 'port', v: String(db.port), mono: true, copy: String(db.port) },
                { k: 'password', v: <SecretValue value={db.password} secret revealed={reveal} onToggle={() => setReveal((r) => !r)} />, mono: true },
                { k: 'url', v: url, mono: true, copy: url.replace('••••••••', db.password) },
              ]} />
              <p className="mt-2 text-[11px] text-muted-foreground">Linked projects get these as <span className="font-mono">{db.engine.toUpperCase().replace('POSTGRESQL', 'DATABASE')}_URL</span>, <span className="font-mono">_HOST</span>, <span className="font-mono">_PORT</span>, <span className="font-mono">_PASSWORD</span> at build and run time.</p>
            </Section>
            <Section title="Runs on">
              <KeyValue compact rows={[{ k: 'image', v: db.image, mono: true, copy: db.image }, { k: 'node', v: <Phrase onClick={() => go(`node:${db.node}`)}>{db.node}</Phrase> }, { k: 'volume', v: db.volume, mono: true }, { k: 'memory limit', v: db.memLimit, mono: true }, { k: 'point-in-time recovery', v: db.pitr ? 'on · 7 days' : 'off', mono: true, state: db.pitr ? undefined : 'idle' }]} />
            </Section>
            <Section title="Linked projects" meta={String(db.linked.length)} action={<a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'link a project', 'picks a project and an environment') }} className="text-xs">link</a>}>
              {db.linked.length ? (
                <ol className="op-rows border bg-background text-xs">{db.linked.map((p) => <li key={p} className="flex items-center justify-between gap-2 px-3 py-2"><span className="flex min-w-0 items-center gap-2"><ProjectMark name={p} icon={PROJECT_ICONS[p]} /><Phrase onClick={() => go(p)}>{p}</Phrase></span><span className="shrink-0 text-muted-foreground"><Link className="inline h-3 w-3" /> production</span></li>)}</ol>
              ) : (
                <p className="border bg-background px-3 py-3 text-xs text-muted-foreground">No project uses this yet. Linking injects the connection variables into the project's environment.</p>
              )}
            </Section>
            <Section title="Danger" meta="typed confirmation">
              <div className="flex flex-wrap gap-2">
                <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><RefreshCw /> restart</Button>} title={`Restart ${db.name}`} description="Clients are disconnected for a few seconds. Data on the volume is kept." confirmWord={db.name} steps={['stop container', 'start container', 'wait for PING']} onDone={() => notify('ok', `${db.name} restarted`)} />
                <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><ArrowUpRight /> upgrade</Button>} title={`Upgrade ${db.name}`} description={`${db.engine} ${db.version} → next minor. A backup is taken first; the service is unavailable for the duration.`} confirmWord={db.name} steps={['back up', 'pull image', 'restart on new image', 'verify']} onDone={() => notify('ok', `${db.name} upgraded`)} />
                <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs text-destructive">delete</Button>} destructive title={`Delete ${db.name}`} description={noBackup ? 'There is no backup. The volume and every key in it are gone for good.' : `The volume is deleted. Backups on ${db.backups[0].source} are kept.`} confirmWord={db.name} steps={['stop container', 'unlink projects', 'delete volume']} onDone={() => go('databases')} />
              </div>
            </Section>
          </div>
        </Columns>
      )}

      {tab === 'backups' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'backup', key: 'id' }, 'status', { label: 'size', key: 'size', numeric: true }, 'source', { label: 'taken', key: 'at' }, '']}
          grid="minmax(6rem,1fr) minmax(12rem,2fr) minmax(70px,max-content) minmax(8rem,1fr) minmax(70px,max-content) minmax(60px,max-content)"
          rows={backupRows} total={db.backups.length} filter={q} onFilter={setQ} placeholder="filter backups"
          state={noBackup ? <PageState state="empty" title="No backups yet" reason="Restores, point-in-time recovery and upgrades all start from a backup. Take one now, or schedule nightly backups from an S3 source." next={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'backup started', `${db.name} → r2-backups`)}><HardDrive /> back up now</Button>} /> : undefined}
          hint={db.pitr ? 'point-in-time recovery: restore to any second in the last 7 days from the backups tab' : 'point-in-time recovery is off for this engine'}
          action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'backup started', `${db.name} → r2-backups`)}><HardDrive /> back up now</Button>}
          footer={<span>nightly at 02:00 · keeps 14 · to r2-backups</span>} />
      )}

      {tab === 'metrics' && (
        <div className="space-y-6">
          <Section title="All metrics" meta={`${db.engine} · last received 6s ago · pick one to chart it`}>
            <div className="space-y-4">{metricStrip(metrics)}{chart(220)}</div>
          </Section>
          <Section title="Resources" meta="container">
            <MetricGrid cols={3}>
              <Metric label="cpu" value="1.0" unit="%" baseline="of 1 core" />
              <Metric label="memory" value="55.4" unit="MB" baseline={`of ${db.memLimit}`} />
              <Metric label="disk" value="1.3" unit="GB" baseline="of 40 GB" />
            </MetricGrid>
          </Section>
        </div>
      )}

      {tab === 'logs' && (
        <Section title="Container logs" meta={live ? 'live · newest at the bottom' : 'paused'} action={<button type="button" className="text-xs text-muted-foreground hover:text-foreground" onClick={() => setLive((l) => !l)}>{live ? 'pause' : 'resume'}</button>}>
          <LogLines lines={LOGS} live={live} height={420} />
        </Section>
      )}

      {tab === 'queries' && (
        db.engine === 'PostgreSQL' ? (
          <div className="space-y-6">
            <Section title="Query time" meta="last 24h · distribution of statement durations">
              <div className="border bg-background p-3"><Histogram buckets={HIST} value={pct} onChange={setPct} /></div>
              <p className="mt-1 font-mono text-[11px] text-muted-foreground">{pct} = {quantile(HIST, pct === 'avg' ? 0.5 : Number(pct.slice(1)) / 100).toFixed(0)} ms · pg_stat_statements</p>
            </Section>
            <Ledger status={null} dense={dense}
              columns={[{ label: 'statement', key: 'q' }, { label: 'calls', key: 'calls', numeric: true }, { label: 'mean', key: 'mean', numeric: true }, { label: 'of total time', key: 'total', numeric: true }, 'why']}
              grid="minmax(16rem,3fr) minmax(70px,max-content) minmax(70px,max-content) minmax(90px,max-content) minmax(10rem,1.5fr)"
              rows={slowRows} total={SLOW.length} filter={q} onFilter={setQ} placeholder="filter statements" hint="◐ mean above 100 ms · × above 500 ms"
              footer={<span>sorted by share of total time · reset counters from the ⋯ menu</span>} />
          </div>
        ) : (
          <div className="border bg-background px-4 py-4 text-xs"><p className="font-medium">Slow queries are a PostgreSQL feature.</p><p className="mt-1 text-muted-foreground">{db.engine} exposes {db.engine === 'Redis' ? 'SLOWLOG' : 'system.query_log'} instead; it will appear here when the collector supports it. Until then, run it from the data browser.</p></div>
        )
      )}

      {tab === 'data' && (() => {
        // Both views are real: the Segmented holds the choice, so picking "query" shows the editor instead of only toasting.
        const word = db.engine === 'Redis' ? 'command' : 'query'
        const example = db.engine === 'Redis' ? 'SCAN 0 MATCH session:* COUNT 100' : db.engine === 'ClickHouse' ? 'SELECT count() FROM events WHERE ts > now() - INTERVAL 1 HOUR' : 'SELECT * FROM public.orders ORDER BY created_at DESC LIMIT 50'
        const run = () => { const text = statement.trim() || example; setRan(text); notify('ok', `ran the ${word}`, `${text.slice(0, 60)} · sandbox: nothing is executed`) }
        return (
          <Section title="Data browser" meta={`${db.engine} · read-only unless you type a write`} action={<Segmented options={[['tree', 'browse'], ['sql', word]] as const} value={dataView} onChange={setDataView} className="h-7 [&>button]:h-7" />}>
            {dataView === 'tree' ? (
              <div className="grid border bg-background text-xs md:grid-cols-[14rem_minmax(0,1fr)]">
                <ol className="op-rows border-b md:border-b-0 md:border-r">
                  {(db.engine === 'Redis' ? ['session:*  1,204', 'ratelimit:*  88', 'queue:emails  1'] : ['public.orders  1.2 GB', 'public.sessions  310 MB', 'public.events  2.4 GB', 'public.audit_logs  110 MB']).map((k) => <li key={k} className="flex justify-between px-3 py-2 font-mono"><span>{k.split('  ')[0]}</span><span className="text-muted-foreground">{k.split('  ')[1]}</span></li>)}
                </ol>
                <div className="px-4 py-6 text-muted-foreground">Pick a {db.engine === 'Redis' ? 'key pattern' : 'table'} on the left. Rows appear here as a grid with the primary key first; edits are staged and applied with ⌘⏎.</div>
              </div>
            ) : (
              <div className="border bg-background text-xs">
                <textarea
                  value={statement} onChange={(e) => setStatement(e.target.value)}
                  onKeyDown={(e) => { if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); run() } }}
                  rows={5} spellCheck={false} placeholder={example} aria-label={`${db.engine} ${word}`}
                  className="block w-full resize-y bg-transparent px-3 py-2 font-mono text-xs outline-none placeholder:text-muted-foreground"
                />
                <div className="flex flex-wrap items-center justify-between gap-2 border-t px-3 py-2">
                  <span className="text-muted-foreground">⌘⏎ runs it · a statement that writes asks before it runs</span>
                  <Button size="sm" className="op-primary h-7 text-xs" onClick={run}>run</Button>
                </div>
                <div className="border-t px-3 py-3 text-muted-foreground">
                  {ran ? <>Ran <span className="font-mono text-foreground">{ran}</span>. This sandbox has no database behind it, so no rows come back; in the console the result grid is here with its row count and duration.</> : <>Nothing has run yet. The result grid appears here with the row count and the duration.</>}
                </div>
              </div>
            )}
          </Section>
        )
      })()}
    </Detail>
  )
}
export const DB_IDS = DBS.map((d) => d.id)

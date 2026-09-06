// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type CSSProperties } from 'react'
import { Cpu, RefreshCw, Server } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Callout, ChartFooter, Columns, Detail, EchoDialog, Field, KeyValue, Ledger, Lede, LogLines, Phrase, Section, Segmented, SecretValue, Settings, Status, StatusLine, TimeChart,
  type KV, type LedgerRow, type LogLine, type State,
} from '@/components/op'
import { Toggle } from './ConsoleV1Admin'
import { EffectLegend, eff } from './ConsoleV1Settings'
import type { Notify } from './ConsoleV1Observe'

/**
 * A node is a machine the fleet runs on. The question the list answers is
 * "is every machine reachable and does any of them hurt", so the second
 * column is the status word with the heartbeat age, not a badge that says
 * Active for a box that stopped answering four minutes ago. Pressure is
 * three numbers in one cell with colour only on the one that is not fine.
 * The record follows the recipe: verdict, lede facts, pressure and what is
 * running as content, how to reach it and its agent in the aside, drain /
 * remove under Danger. Cluster-wide settings (join token, DNS, trust) are
 * a settings page of their own, not a card stacked under the list.
 */

// ── Data ─────────────────────────────────────────────────────────────
type NodeStatus = 'online' | 'offline' | 'draining'
type Node = {
  name: string; role: 'control plane' | 'worker'; reach: 'local' | 'direct' | 'relay'; status: NodeStatus
  heartbeat: string; address: string; publicAddress?: string; arch: string; os: string; agent: string; joined: string; up: string
  vcpu: number; mem: string; disk: string; cpu: number; memPct: number; diskPct: number; load: string
  containers: { name: string; project: string; kind: string; state: State; mem: string }[]
}
export const NODES: Node[] = [
  { name: 'hetzner-1', role: 'control plane', reach: 'local', status: 'online', heartbeat: '2s ago', address: '10.0.3.1', publicAddress: '91.107.201.10', arch: 'amd64', os: 'Ubuntu 24.04', agent: 'v0.1.0 (built in)', joined: '2026-03-02', up: '41d', vcpu: 3, mem: '4 GB', disk: '80 GB', cpu: 11, memPct: 91, diskPct: 41, load: '0.8',
    containers: [
      { name: 'acme-storefront-dep_91a-1', project: 'acme-storefront', kind: 'app', state: 'ok', mem: '312 MB' }, { name: 'acme-storefront-dep_91a-2', project: 'acme-storefront', kind: 'app', state: 'ok', mem: '298 MB' },
      { name: 'api-gateway-dep_87c-1', project: 'api-gateway', kind: 'app', state: 'ok', mem: '210 MB' }, { name: 'acme-pg', project: 'acme-pg', kind: 'postgres', state: 'ok', mem: '1.6 GB' },
      { name: 'sessions-redis', project: 'sessions-redis', kind: 'redis', state: 'warn', mem: '241 MB of 256' }, { name: 'temps-preview-gateway', project: 'system', kind: 'system', state: 'ok', mem: '48 MB' },
    ] },
  { name: 'hetzner-2', role: 'worker', reach: 'direct', status: 'online', heartbeat: '11s ago', address: '10.0.3.2', publicAddress: '91.107.201.11', arch: 'amd64', os: 'Ubuntu 24.04', agent: 'v0.1.0', joined: '2026-05-14', up: '12d', vcpu: 4, mem: '8 GB', disk: '160 GB', cpu: 6, memPct: 38, diskPct: 22, load: '0.3',
    containers: [
      { name: 'events-ch', project: 'events-ch', kind: 'clickhouse', state: 'ok', mem: '2.1 GB' }, { name: 'acme-crm-dep_44a-1', project: 'acme-crm', kind: 'app', state: 'ok', mem: '180 MB' },
      { name: 'acme-crm-dep_44a-2', project: 'acme-crm', kind: 'app', state: 'ok', mem: '176 MB' }, { name: 'docs-dep_12b-1', project: 'docs', kind: 'static', state: 'ok', mem: '22 MB' },
    ] },
  { name: 'hetzner-3', role: 'worker', reach: 'relay', status: 'offline', heartbeat: '4m ago', address: '10.0.3.3', arch: 'arm64', os: 'Ubuntu 24.04', agent: 'v0.0.9', joined: '2026-08-20', up: '—', vcpu: 2, mem: '4 GB', disk: '40 GB', cpu: 0, memPct: 0, diskPct: 0, load: '—',
    containers: [
      { name: 'billing-worker-dep_31c-1', project: 'billing-worker', kind: 'app', state: 'error', mem: '—' }, { name: 'billing-worker-dep_31c-2', project: 'billing-worker', kind: 'app', state: 'error', mem: '—' }, { name: 'nightly-report', project: 'acme-storefront', kind: 'cron', state: 'error', mem: '—' },
    ] },
]
const NODE_STATE: Record<NodeStatus, State> = { online: 'ok', offline: 'error', draining: 'warn' }
const pressureState = (n: Node): State => n.status === 'offline' ? 'idle' : n.memPct >= 90 || n.diskPct >= 90 || n.cpu >= 90 ? 'warn' : 'ok'
const pct = (v: number, offline: boolean) => offline ? '—' : `${v}%`

const series = (base: number, amp: number, seed: number) => Array.from({ length: 48 }, (_, i) => +(base + Math.abs(Math.sin((i + seed) / 4.2)) * amp).toFixed(1))
const L = (t: string, level: LogLine['level'], msg: string): LogLine => ({ t, level, source: 'agent', msg })
const AGENT_LOG: Record<NodeStatus, LogLine[]> = {
  online: [L('20:41:02', 'info', 'heartbeat ok · 6 containers · mem 91%'), L('20:41:02', 'warn', 'memory above 90% for 18 min · sessions-redis at its limit'), L('20:40:47', 'info', 'heartbeat ok'), L('20:40:32', 'info', 'heartbeat ok'), L('20:39:10', 'info', 'pulled sha256:9e21c7… for acme-storefront (212 MB, 14s)'), L('20:38:55', 'debug', 'gc: removed 3 stopped containers, 410 MB')],
  draining: [L('20:41:02', 'info', 'draining: 2 of 6 containers moved to hetzner-2')],
  offline: [L('20:37:14', 'error', 'control plane unreachable: wss://app.acme.sh/agent timed out after 30s (relay)'), L('20:36:44', 'warn', 'heartbeat failed, retry 1 of 3'), L('20:36:29', 'info', 'heartbeat ok · 3 containers · mem 44%'), L('20:36:14', 'info', 'heartbeat ok'), L('20:35:59', 'info', 'heartbeat ok')],
}

// ── Ledger (settings:nodes) ───────────────────────────────────────────
export function NodesLedger({ dense, go, meta }: { dense: boolean; go: (v: string) => void; meta: React.ReactNode }) {
  const [q, setQ] = useState('')
  const offline = NODES.filter((n) => n.status === 'offline')
  const hot = NODES.filter((n) => n.status === 'online' && pressureState(n) === 'warn')
  const rows: LedgerRow[] = NODES.filter((n) => n.name.toLowerCase().includes(q.trim().toLowerCase()) || n.role.toLowerCase().includes(q.trim().toLowerCase())).map((n) => {
    const off = n.status === 'offline'
    const worst: 'cpu' | 'mem' | 'disk' | null = off ? null : n.memPct >= 90 ? 'mem' : n.diskPct >= 90 ? 'disk' : n.cpu >= 90 ? 'cpu' : null
    const pressure = (
      <span className={off ? 'text-muted-foreground' : undefined}>
        <span className={worst === 'cpu' ? 'text-warning' : undefined}>cpu {pct(n.cpu, off)}</span> · <span className={worst === 'mem' ? 'text-warning' : undefined}>mem {pct(n.memPct, off)}</span> · <span className={worst === 'disk' ? 'text-warning' : undefined}>disk {pct(n.diskPct, off)}</span>
      </span>
    )
    return {
      // Brand §6 "an icon wherever it adds context": the fleet mixes roles, so the role leads the row.
      // Muted ink; the status glyph keeps its own slot, so an offline worker reads × and a chip, never a red chip.
      id: n.name, state: NODE_STATE[n.status], onOpen: () => go(`node:${n.name}`), icon: n.role === 'control plane' ? <Server aria-hidden /> : <Cpu aria-hidden />,
      sort: { name: n.name, status: n.status, heartbeat: n.heartbeat, mem: off ? null : n.memPct, running: n.containers.length },
      mobile: <><span className="block font-medium">{n.name} <span className="font-normal text-muted-foreground">· {n.role}</span></span><span className="block truncate text-[11px] text-muted-foreground">{n.status} · heartbeat {n.heartbeat} · {off ? `${n.containers.length} containers unreachable` : `mem ${n.memPct}%`}</span></>,
      cells: [
        <span className="font-medium">{n.name}</span>,
        <Status state={NODE_STATE[n.status]} label={`${n.status} · ${n.heartbeat}`} />,
        <span className="text-muted-foreground">{n.role}{n.role === 'worker' && ` · ${n.reach}`}</span>,
        <span className="font-mono text-muted-foreground">{n.address}</span>,
        <span className="font-mono text-muted-foreground">{n.vcpu} vCPU · {n.mem}</span>,
        <span className="font-mono">{pressure}</span>,
        <span className={off ? 'text-destructive' : 'text-muted-foreground'}>{n.containers.length} {off ? 'unreachable' : 'containers'}</span>,
      ],
    }
  })
  const status = offline.length
    ? <StatusLine state="error" more={hot.length ? { label: `+${hot.length}`, items: hot.map((n) => ({ state: 'warn' as State, children: <><Phrase onClick={() => go(`node:${n.name}`)}>{n.name}</Phrase> is at {n.memPct}% memory; sessions-redis is at its limit. Move a service to hetzner-2 or join a node.</> })) } : undefined}>
        <Phrase onClick={() => go(`node:${offline[0].name}`)}>{offline[0].name}</Phrase> has not sent a heartbeat for 4 minutes. Its {offline[0].containers.length} containers are unreachable and the proxy answers 502 for billing-worker. Check the agent on the machine, or drain it to move the work.
      </StatusLine>
    : hot.length
      ? <StatusLine state="warn"><Phrase onClick={() => go(`node:${hot[0].name}`)}>{hot[0].name}</Phrase> is at {hot[0].memPct}% memory. Move a service to another node or join one.</StatusLine>
      : <StatusLine state="ok">Every node answered in the last 15 seconds. Nothing is under pressure.</StatusLine>
  return (
    <Ledger title="Nodes" meta={meta} dense={dense} status={status}
      columns={[{ label: 'node', key: 'name' }, { label: 'status', key: 'status' }, 'role', 'address', 'size', { label: 'pressure', key: 'mem' }, { label: 'running', key: 'running', numeric: true }]}
      grid="minmax(7rem,max-content) minmax(9rem,max-content) minmax(8rem,max-content) minmax(6rem,max-content) minmax(0,1fr) minmax(12rem,max-content) minmax(7rem,max-content)"
      rows={rows} total={NODES.length} filter={q} onFilter={setQ} placeholder="filter nodes"
      hint={<>heartbeat every 15s · offline after 3 missed · <Phrase onClick={() => go('settings:cluster')}>cluster: dns on · join token valid 23h</Phrase></>}
      action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => go('settings:cluster')}>join a node</Button>}
      footer={<span>× offline: no heartbeat for 45s · ◐ draining or under pressure · colour on the one number that is not fine</span>} />
  )
}

// ── Record (node:<name>) ──────────────────────────────────────────────
type Tab = 'overview' | 'containers' | 'agent log'
const TABS = ['overview', 'containers', 'agent log'] as const
type Dim = 'cpu' | 'mem' | 'disk'

export function NodeScreen({ name, dense, notify, go }: { name: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const n = NODES.find((x) => x.name === name) ?? NODES[0]
  const [tab, setTab] = useState<Tab>('overview')
  const [dim, setDim] = useState<Dim>(n.memPct >= 90 ? 'mem' : 'cpu')
  const [range, setRange] = useState<'1h' | '6h' | '24h' | '7d'>('6h')
  const [q, setQ] = useState('')
  const off = n.status === 'offline'
  const DIMS: { key: Dim; label: string; value: string; sub: string; state?: State; series: number[] }[] = [
    { key: 'cpu', label: 'cpu', value: pct(n.cpu, off), sub: `${n.vcpu} vCPU · load ${n.load}`, series: series(n.cpu - 4, 9, 1) },
    { key: 'mem', label: 'memory', value: pct(n.memPct, off), sub: `of ${n.mem}`, state: !off && n.memPct >= 90 ? 'warn' : undefined, series: series(n.memPct - 6, 8, 3) },
    { key: 'disk', label: 'disk', value: pct(n.diskPct, off), sub: `of ${n.disk}`, state: !off && n.diskPct >= 90 ? 'warn' : undefined, series: series(n.diskPct - 1, 1.5, 5) },
  ]
  const d = DIMS.find((x) => x.key === dim) ?? DIMS[0]

  const status = off
    ? <StatusLine state="error">No heartbeat for 4 minutes; the last one was at 20:36. The relay connection timed out, so the {n.containers.length} containers on it are unreachable and billing-worker answers 502. Check the agent on the machine (<span className="font-mono">systemctl status temps-agent</span>) or drain it to move the work.</StatusLine>
    : n.status === 'draining'
      ? <StatusLine state="warn">Draining: containers are moving to other nodes. New deploys skip this node until you undrain it.</StatusLine>
      : n.memPct >= 90
        ? <StatusLine state="warn">Memory is at {n.memPct}% of {n.mem} and has been above 90% for 18 minutes. <Phrase onClick={() => go('db:sessions-redis')}>sessions-redis</Phrase> is at its limit and restarted once yesterday. Move a service to hetzner-2, or join a node.</StatusLine>
        : <StatusLine state="ok">Nothing to do: heartbeat {n.heartbeat}, nothing under pressure, {n.containers.length} containers running.</StatusLine>

  const facts: KV[] = [
    { k: 'heartbeat', v: n.heartbeat, mono: true, state: off ? 'error' : undefined },
    { k: 'address', v: n.address, mono: true, copy: n.address },
    { k: 'reach', v: n.reach === 'local' ? 'this machine' : `wireguard · ${n.reach}`, mono: true },
    { k: 'agent', v: n.agent, mono: true, state: n.agent.startsWith('v0.0.') ? 'warn' : undefined },
    { k: 'running', v: off ? `${n.containers.length} unreachable` : `${n.containers.length} containers`, mono: true, state: off ? 'error' : undefined },
    { k: 'up', v: n.up, mono: true },
  ]
  const lede = (
    <Lede state={NODE_STATE[n.status]} word={n.status} facts={facts}>
      {n.vcpu} vCPU · {n.mem} · {n.disk} disk · {off ? 'no samples since 20:37' : `memory ${n.memPct}%`}
    </Lede>
  )

  const containerRows: LedgerRow[] = n.containers.filter((c) => c.name.toLowerCase().includes(q.trim().toLowerCase()) || c.project.toLowerCase().includes(q.trim().toLowerCase())).map((c) => ({
    id: c.name, state: c.state, onOpen: () => go(c.kind === 'app' || c.kind === 'static' || c.kind === 'cron' ? c.project : c.kind === 'system' ? 'settings' : `db:${c.project}`),
    sort: { name: c.name, project: c.project, kind: c.kind },
    mobile: <><span className="block font-mono">{c.name}</span><span className="block text-[11px] text-muted-foreground">{c.project} · {c.kind} · {c.mem}</span></>,
    cells: [<span className="font-mono"><Status state={c.state} label={c.name} /></span>, <span>{c.project}</span>, <span className="text-muted-foreground">{c.kind}</span>, <span className="font-mono text-muted-foreground">{c.mem}</span>],
  }))

  return (
    <Detail title={n.name} meta={`${n.role} · ${n.arch} · joined ${n.joined}`} status={status} lede={lede} tabs={TABS} tab={tab} onTab={setTab}
      actions={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', off ? 'pinging hetzner-3' : 'checked', off ? 'relay handshake timed out after 5s' : `heartbeat now · ${n.containers.length} containers`)}><RefreshCw /> check now</Button>}>
      {tab === 'overview' && (
        <Columns>
          <div>
            {off && (
              <Callout state="error" title="The agent stopped answering at 20:37" quote="control plane unreachable: wss://app.acme.sh/agent timed out after 30s (relay)" action={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => setTab('agent log')}>agent log</Button>}>
                The last five heartbeats before that were fine. A relay node needs outbound 443 to the control plane; a direct node needs UDP 51820 both ways. The containers keep running on the machine, but the proxy cannot reach them.
              </Callout>
            )}
            <Section title="Pressure" meta={off ? 'last known values · 4m old' : `sampled every 15s · ${range}`}>
              <div className="space-y-4">
                <div className="op-tiles" style={{ '--tiles': 3 } as CSSProperties}>
                  {DIMS.map((x) => { const on = x.key === dim; return (
                    <button key={x.key} type="button" aria-pressed={on} onClick={() => setDim(x.key)} className={`min-w-0 p-3 text-left transition-colors hover:bg-muted/40 ${on ? 'bg-muted/60' : ''}`}>
                      <p className="op-label truncate">{x.label}</p>
                      <p className={`mt-1 flex items-baseline gap-2 font-mono text-lg leading-6 ${off ? 'text-muted-foreground' : ''}`}>{x.value}{x.state && <span className="text-xs"><Status state={x.state} label="over 90%" /></span>}</p>
                      <p className="truncate font-mono text-[11px] text-muted-foreground">{x.sub}</p>
                    </button>
                  ) })}
                </div>
                <div className="border bg-background p-3">
                  <TimeChart data={d.series.map((v, i) => ({ t: `${String(14 + Math.floor(i / 8)).padStart(2, '0')}:${String((i % 8) * 7.5).padStart(2, '0').slice(0, 2)}`, v: off ? 0 : v }))} series={[{ key: 'v', name: d.label }]} unit="%" height={160} xInterval={7} readoutFormat={(p) => `${p.t} · ${d.label} ${p.v}%`} />
                </div>
                <div className="flex flex-wrap items-center justify-between gap-2"><ChartFooter><span>{d.label} · {range}{off && ' · flat since 20:37, no samples'}</span></ChartFooter><Segmented options={[['1h', '1h'], ['6h', '6h'], ['24h', '24h'], ['7d', '7d']] as const} value={range} onChange={setRange} className="h-6 [&>button]:h-6" /></div>
              </div>
            </Section>
            <Section title="Running" meta={`${n.containers.length} containers${off ? ' · unreachable' : ''}`} action={<a href="#" onClick={(e) => { e.preventDefault(); setTab('containers') }} className="text-xs">all containers</a>}>
              <ol className="op-rows border bg-background text-xs">
                {n.containers.slice(0, 4).map((c) => <li key={c.name} className="flex items-center justify-between gap-3 px-3 py-2"><span className="min-w-0 truncate font-mono"><Status state={c.state} label={c.name} /></span><span className="shrink-0 font-mono text-muted-foreground">{c.mem}</span></li>)}
                {n.containers.length > 4 && <li className="px-3 py-2 text-muted-foreground">and {n.containers.length - 4} more</li>}
              </ol>
            </Section>
          </div>
          <div>
            <Section title="Reach" meta={n.reach === 'local' ? 'this machine' : `wireguard · ${n.reach}`}>
              <KeyValue compact rows={[
                ...(n.publicAddress ? [{ k: 'public address', v: n.publicAddress, mono: true, copy: n.publicAddress }] : []),
                { k: 'tunnel', v: n.reach === 'local' ? 'none' : n.reach === 'direct' ? 'UDP 51820 both ways' : 'relay over 443 through the control plane', mono: true },
                { k: 'latency', v: off ? '—' : n.reach === 'local' ? '0 ms' : n.reach === 'direct' ? '0.4 ms' : '9 ms', mono: true },
              ]} />
            </Section>
            <Section title="Agent">
              {/* The version is a lede fact; the aside says only what the lede does not. */}
              <KeyValue compact rows={[{ k: 'os', v: n.os, mono: true }]} />
              {n.agent.startsWith('v0.0.') && <p className="mt-2 text-[11px] text-muted-foreground">The agent version above is one behind the control plane. It still works; update it on the machine with <span className="font-mono">temps agent update</span>.</p>}
            </Section>
            <Section title="Danger" meta="typed confirmation">
              <div className="flex flex-wrap gap-2">
                {n.role === 'worker' && n.status !== 'draining' && <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs">drain</Button>} title={`Drain ${n.name}`} description={`Its ${n.containers.length} containers are redeployed on other nodes, one at a time. New deploys skip ${n.name} until you undrain it.`} confirmWord={n.name} steps={['mark unschedulable', 'redeploy containers elsewhere', 'wait for health']} onDone={() => notify('ok', `${n.name} drained`, `${n.containers.length} containers moved`)} />}
                {n.status === 'draining' && <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', `${n.name} undrained`)}>undrain</Button>}
                {n.role === 'worker' && <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs text-destructive">remove</Button>} destructive title={`Remove ${n.name} from the fleet`} description={off ? `The node is offline, so its ${n.containers.length} containers cannot be moved first: they are redeployed elsewhere and whatever is still on the machine is orphaned.` : `Drains first, then revokes its tunnel key. The machine keeps running; temps forgets it.`} confirmWord={n.name} steps={off ? ['redeploy containers elsewhere', 'revoke tunnel key', 'forget node'] : ['drain', 'revoke tunnel key', 'forget node']} onDone={() => { notify('warn', `${n.name} removed`); go('settings:nodes') }} />}
                {n.role === 'control plane' && <p className="text-[11px] text-muted-foreground">The control plane cannot be drained or removed; it is this instance.</p>}
              </div>
            </Section>
          </div>
        </Columns>
      )}

      {tab === 'containers' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'container', key: 'name' }, { label: 'project', key: 'project' }, { label: 'kind', key: 'kind' }, { label: 'memory', numeric: true }]}
          grid="minmax(14rem,2fr) minmax(8rem,1fr) minmax(5rem,max-content) minmax(7rem,max-content)"
          rows={containerRows} total={n.containers.length} filter={q} onFilter={setQ} placeholder="filter containers"
          hint={off ? '× unreachable since 20:37 · they may still be running on the machine' : 'open a row for the project or service it belongs to'}
          footer={<span>{off ? 'the proxy stops routing to a node after 3 missed heartbeats' : `${n.containers.filter((c) => c.kind === 'app').length} app · ${n.containers.filter((c) => c.kind !== 'app' && c.kind !== 'system').length} services · ${n.containers.filter((c) => c.kind === 'system').length} system`}</span>} />
      )}

      {tab === 'agent log' && (
        <Section title="Agent log" meta={off ? 'last lines received · nothing since 20:37' : 'live · newest first'}>
          <LogLines lines={AGENT_LOG[n.status]} live={!off} height={420} search />
        </Section>
      )}
    </Detail>
  )
}

// ── Cluster settings (settings:cluster) ───────────────────────────────
export function ClusterPage({ meta, notify }: { meta: React.ReactNode; notify: Notify }) {
  const [dirty, setDirty] = useState(false)
  const [dns, setDns] = useState(true)
  const [reveal, setReveal] = useState(false)
  const touch = () => setDirty(true)
  const token = 'tj_4Kq9…vX2m'
  return (
    <div className="space-y-4">
    <Settings title="Cluster" meta={meta} status={<StatusLine state="ok">Nothing to do: the join token is valid for 23h, cluster DNS is on, and both workers trust the CA from March.</StatusLine>}
      onSave={() => { setDirty(false); notify('ok', 'cluster settings saved') }} dirty={dirty}
      sections={[
        { title: 'joining', body: <>
          <Field label="join token" help="valid 23h more · a machine needs it once, at join · regenerate invalidates it for machines that have not joined yet">
            <span className="flex flex-wrap items-center gap-2"><SecretValue value="tj_4Kq9f2Lm8Rt1vX2m" secret revealed={reveal} onToggle={() => setReveal((r) => !r)} /><Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'new join token', 'valid 24h · the old one stops working now')}>regenerate</Button></span>
          </Field>
          <div className="grid min-w-0 gap-1 text-xs">
            <p className="op-label">on the machine</p>
            <pre className="op-inset min-w-0 max-w-full overflow-x-auto px-3 py-2 font-mono text-[11px] leading-5">{`curl -fsSL https://temps.sh/install.sh | bash\ntemps join https://app.acme.sh ${reveal ? 'tj_4Kq9f2Lm8Rt1vX2m' : token} --private-address <worker-ip>\ntemps agent`}</pre>
            <p className="text-[11px] text-muted-foreground">install, join with the token and the machine's private address, start the agent · direct needs UDP 51820 both ways, relay needs outbound 443 only</p>
          </div>
        </> },
        { title: 'cluster dns', body: <>
          <Field label="cluster dns" help={eff('restart', 'containers resolve *.temps.local, needed for service-to-service traffic such as primary.pg-orders.temps.local · containers that are already running pick it up when they are next deployed')}><Toggle checked={dns} onChange={(v) => { setDns(v); touch() }} /></Field>
          <Field label="pool cidr" help="locked after 2 allocations · changing an active pool is a cluster network migration, so it cannot be edited here"><Input defaultValue="172.20.0.0/16" disabled className="h-8 w-48 font-mono text-xs" /></Field>
          <Field label="per-node prefix" help="one subnet from the pool per node · /24 gives 254 containers per node"><Input defaultValue="24" disabled className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
        { title: 'trust', body: <>
          <Field label="cluster ca" help="issued 2026-03-02 · authenticates the control plane and every worker · 2 workers trust it"><span className="block break-all font-mono text-[11px]">24590d5ac6f5d0537ca2bdf96c1602be0539f8086774424c1bd821309c6971ad</span></Field>
        </> },
      ]}
      danger={<div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Rotate the cluster CA</p><p className="text-[11px] text-muted-foreground">Emergency only. Every worker stops trusting the control plane at once and has to be re-joined by hand; outstanding join tokens die with it.</p></div><EchoDialog trigger={<Button size="sm" variant="outline" className="h-8 text-xs text-destructive">rotate ca</Button>} destructive title="Rotate the cluster CA" description="2 workers lose trust immediately and their containers keep running unreachable until each is re-joined. Type rotate to confirm." confirmWord="rotate" steps={['issue new ca', 'revoke old ca', 'invalidate join tokens']} onDone={() => notify('warn', 'cluster ca rotated', '2 workers need to re-join')} /></div>} />
    <EffectLegend />
    </div>
  )
}

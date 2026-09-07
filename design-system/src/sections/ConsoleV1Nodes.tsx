// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Cpu, RefreshCw, Server } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Detail, EchoDialog, Field, Ledger, LogLines, Phrase, STATE_RANK, Section, SecretValue, Settings, Sparkline, Status, StatusLine, Topology, fmtBytes, fmtPct,
  type LedgerRow, type LogLine, type State, type TopoLink, type TopoNode,
} from '@/components/op'
import { NodeLede, NodeResources, containersOf, nodeVerdict, pressureRank, readingsOf, resOf, sparksOf, type NodeFacts } from './ConsoleV1Resources'
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
/* A node is identity and reach; what it is *using* lives in
   `ConsoleV1Resources.tsx`, with the containers, so the ledger, the record and
   the cluster graph read one roster and one set of samples. */
type NodeStatus = 'online' | 'offline' | 'draining'
type Node = {
  name: string; role: 'control plane' | 'worker'; region: string; reach: 'local' | 'direct' | 'relay'; status: NodeStatus
  heartbeat: string; address: string; publicAddress?: string; arch: string; os: string; agent: string; joined: string; up: string
}
export const NODES: Node[] = [
  { name: 'hetzner-1', role: 'control plane', region: 'fsn1', reach: 'local', status: 'online', heartbeat: '2s ago', address: '10.0.3.1', publicAddress: '203.0.113.10', arch: 'amd64', os: 'Ubuntu 24.04', agent: 'v0.1.0 (built in)', joined: '2026-03-02', up: '41d' },
  { name: 'hetzner-2', role: 'worker', region: 'fsn1', reach: 'direct', status: 'online', heartbeat: '11s ago', address: '10.0.3.2', publicAddress: '203.0.113.11', arch: 'amd64', os: 'Ubuntu 24.04', agent: 'v0.1.0', joined: '2026-05-14', up: '12d' },
  { name: 'hetzner-3', role: 'worker', region: 'nbg1', reach: 'relay', status: 'offline', heartbeat: '4m ago', address: '10.0.3.3', arch: 'arm64', os: 'Ubuntu 24.04', agent: 'v0.0.9', joined: '2026-08-20', up: '–' },
]
const NODE_STATE: Record<NodeStatus, State> = { online: 'ok', offline: 'error', draining: 'warn' }
/** The pressure a row is ranked and toned by: the worst of the three, from the same samples the record plots. */
const worstOf = (n: Node) => {
  const rd = readingsOf(n.name)
  const three = [{ k: 'cpu' as const, v: rd.cpuNow }, { k: 'memory' as const, v: rd.memNow }, { k: 'disk' as const, v: rd.diskPct }]
  return three.sort((a, b) => b.v - a.v)[0]
}
const pressureState = (n: Node): State => {
  if (n.status === 'offline') return 'idle'
  const w = worstOf(n)
  return w.v >= 90 ? 'warn' : 'ok'
}

const L = (t: string, level: LogLine['level'], msg: string): LogLine => ({ t, level, source: 'agent', msg })
/* The agent log is the record's other facet, so it is written from the same
   node and the same clock the resources are: a log that says 91% on a machine
   sitting at 58% is the fastest way to make a console look like a mock. */
const agentLog = (n: Node): LogLine[] => {
  const running = containersOf(n.name).length
  if (n.status === 'offline') return [
    L('21:25:14', 'error', 'control plane unreachable: wss://app.acme.sh/agent timed out after 30s (relay)'),
    L('21:24:44', 'warn', 'heartbeat failed, retry 1 of 3'),
    L('21:24:29', 'info', `heartbeat ok · ${running} containers · mem 44%`),
    L('21:24:14', 'info', 'heartbeat ok'),
    L('21:23:59', 'info', 'heartbeat ok'),
  ]
  if (n.status === 'draining') return [L('21:29:02', 'info', `draining: 2 of ${running} containers moved to hetzner-1`)]
  const rd = readingsOf(n.name)
  return [
    L('21:29:02', 'info', `heartbeat ok · ${running} containers · mem ${Math.round(rd.memNow)}%`),
    ...(rd.memNow >= 85 ? [L('21:28:47', 'warn', 'memory above 85% for 1h 59m · billing-worker-dep_31c-1 at 1.4 GiB and still growing')] : [L('21:28:47', 'info', 'heartbeat ok')]),
    L('21:28:32', 'info', 'heartbeat ok'),
    L('21:27:10', 'info', 'pulled sha256:9e21c7… for api-gateway (212 MB, 14s)'),
    L('21:26:55', 'debug', 'gc: removed 3 stopped containers, 410 MB'),
  ]
}

// ── Ledger (settings:nodes) ───────────────────────────────────────────

/** The four shapes in one cell: a row is compared to the row above it by shape, not by reading four numbers. */
function Sparks({ node, off }: { node: string; off: boolean }) {
  const s = sparksOf(node)
  const rd = readingsOf(node)
  const one = (label: string, points: number[], state?: State) => (
    <span key={label} className="block min-w-0 flex-1" title={`${label} · 24h`}>
      <span className="sr-only">{label} over 24h</span>
      <Sparkline points={points} height={16} state={state} />
    </span>
  )
  return (
    <span className={`flex min-w-0 items-center gap-1.5 ${off ? 'text-muted-foreground opacity-70' : 'text-muted-foreground'}`}>
      {one('cpu', s.cpu, rd.cpuNow >= 80 ? 'warn' : undefined)}
      {one('memory', s.mem, rd.memNow >= 85 ? 'warn' : undefined)}
      {one('disk', s.disk, rd.diskPct >= 80 ? 'warn' : undefined)}
      {one('network', s.net)}
    </span>
  )
}

export function NodesLedger({ dense, go, meta }: { dense: boolean; go: (v: string) => void; meta: React.ReactNode }) {
  const [q, setQ] = useState('')
  const offline = NODES.filter((n) => n.status === 'offline')
  const hot = NODES.filter((n) => n.status === 'online' && pressureState(n) === 'warn')
  // Pressure first: an unreachable machine, then the one closest to a threshold.
  // The order is the answer to "which machine do I look at", so it is the default and the hint says so.
  const ordered = [...NODES].sort((a, b) => STATE_RANK[NODE_STATE[a.status]] - STATE_RANK[NODE_STATE[b.status]] || pressureRank(a.name, a.status === 'offline') - pressureRank(b.name, b.status === 'offline'))
  const rows: LedgerRow[] = ordered.filter((n) => n.name.toLowerCase().includes(q.trim().toLowerCase()) || n.role.toLowerCase().includes(q.trim().toLowerCase())).map((n) => {
    const off = n.status === 'offline'
    const rd = readingsOf(n.name)
    const w = worstOf(n)
    const running = containersOf(n.name).length
    const tone = (k: string, v: number, warnAt: number) => (
      <span className={off ? 'text-muted-foreground' : w.k === k && v >= warnAt ? 'text-warning' : undefined}>{k === 'memory' ? 'mem' : k} {fmtPct(v, { digits: 0 })}</span>
    )
    // The one fact a phone has room for: what is worst on this machine right now.
    const worstFact = off ? `no heartbeat for ${n.heartbeat.replace(' ago', '')} · last sample ${rd.stale}` : `${w.k} ${fmtPct(w.v, { digits: 0 })}`
    return {
      // Brand §6 "an icon wherever it adds context": the fleet mixes roles, so the role leads the row.
      // Muted ink; the status glyph keeps its own slot, so an offline worker reads × and a chip, never a red chip.
      id: n.name, state: NODE_STATE[n.status], onOpen: () => go(`node:${n.name}`), icon: n.role === 'control plane' ? <Server aria-hidden /> : <Cpu aria-hidden />,
      sort: { name: n.name, status: n.status, heartbeat: n.heartbeat, mem: off ? null : rd.memNow, running },
      mobile: <><span className="block font-medium">{n.name} <span className="font-normal text-muted-foreground">· {n.role}</span></span><span className="block truncate text-[11px] text-muted-foreground">{worstFact}</span></>,
      cells: [
        <span className="font-medium">{n.name}</span>,
        <Status state={NODE_STATE[n.status]} label={`${n.status} · ${n.heartbeat}`} />,
        <span className="text-muted-foreground">{n.role}{n.role === 'worker' && ` · ${n.reach}`}</span>,
        <span className="font-mono text-muted-foreground">{n.address}</span>,
        <span className="font-mono text-muted-foreground">{rd.vcpu} vCPU · {fmtBytes(rd.memTotal, { binary: true })}</span>,
        <span className="font-mono">{tone('cpu', rd.cpuNow, 80)} · {tone('memory', rd.memNow, 85)} · {tone('disk', rd.diskPct, 80)}</span>,
        <Sparks node={n.name} off={off} />,
        <span className={off ? 'text-destructive' : 'text-muted-foreground'}>{running} {off ? 'unreachable' : 'containers'}</span>,
      ],
    }
  })
  const status = offline.length
    ? <StatusLine state="error" more={hot.length ? { label: `+${hot.length}`, items: hot.map((n) => ({ state: 'warn' as State, children: <><Phrase onClick={() => go(`node:${n.name}`)}>{n.name}</Phrase> is at {fmtPct(readingsOf(n.name).memNow, { digits: 0 })} memory and still climbing since dep_31c. Move billing-worker to hetzner-1 or join a node.</> })) } : undefined}>
        <Phrase onClick={() => go(`node:${offline[0].name}`)}>{offline[0].name}</Phrase> has not sent a heartbeat for 4 minutes. Its {containersOf(offline[0].name).length} containers are unreachable and the proxy answers 502 for billing-worker. Check the agent on the machine, or drain it to move the work.
      </StatusLine>
    : hot.length
      ? <StatusLine state="warn"><Phrase onClick={() => go(`node:${hot[0].name}`)}>{hot[0].name}</Phrase> is at {fmtPct(readingsOf(hot[0].name).memNow, { digits: 0 })} memory. Move a service to another node or join one.</StatusLine>
      : <StatusLine state="ok">Every node answered in the last 15 seconds. Nothing is under pressure.</StatusLine>
  return (
    <Ledger title="Nodes" meta={meta} dense={dense} status={status}
      columns={[{ label: 'node', key: 'name' }, { label: 'status', key: 'status' }, 'role', 'address', 'size', { label: 'pressure', key: 'mem' }, 'cpu · mem · disk · net · 24h', { label: 'running', key: 'running', numeric: true }]}
      grid="minmax(7rem,max-content) minmax(9rem,max-content) minmax(8rem,max-content) minmax(6rem,max-content) minmax(9rem,max-content) minmax(12rem,max-content) minmax(8rem,1fr) minmax(7rem,max-content)"
      rows={rows} total={NODES.length} filter={q} onFilter={setQ} placeholder="filter nodes"
      hint={<>pressure first: unreachable, then closest to a threshold · <Phrase onClick={() => go('settings:cluster')}>cluster: dns on · join token valid 23h</Phrase></>}
      action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => go('settings:cluster')}>join a node</Button>}
      footer={<span>× offline: no heartbeat for 45s · ◐ draining or above a threshold · sparklines are the same 24h the record plots · colour on the one number that is not fine</span>} />
  )
}

// ── Record (node:<name>) ──────────────────────────────────────────────
/* Two facets and no more: the machine (§7b Resources) and the tool that says
   what it has been telling us. "Containers" folded into the machine, under the
   charts, because the ledger is the attribution the charts owe the reader. */
type Tab = 'resources' | 'agent log'
const TABS = ['resources', 'agent log'] as const

export function NodeScreen({ name, dense, notify, go }: { name: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const n = NODES.find((x) => x.name === name) ?? NODES[0]
  const [tab, setTab] = useState<Tab>('resources')
  const off = n.status === 'offline'
  const facts: NodeFacts = { name: n.name, role: n.role, reach: n.reach, arch: n.arch, offline: off, draining: n.status === 'draining', heartbeat: n.heartbeat, agent: n.agent }
  const running = containersOf(n.name).length
  return (
    <Detail title={n.name} meta={`${n.role} · ${n.region} · ${n.reach === 'local' ? 'this machine' : n.reach}`}
      status={nodeVerdict(facts, go)} lede={<NodeLede node={facts} />} tabs={TABS} tab={tab} onTab={setTab}
      actions={<>
        <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', off ? `pinging ${n.name}` : 'checked', off ? 'relay handshake timed out after 5s' : `heartbeat now · ${running} containers`)}><RefreshCw /> check now</Button>
        {n.role === 'worker' && <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs text-destructive">remove</Button>} destructive title={`Remove ${n.name} from the fleet`} description={off ? `The node is offline, so its ${running} containers cannot be moved first: they are redeployed elsewhere and whatever is still on the machine is orphaned.` : 'Drains first, then revokes its tunnel key. The machine keeps running; temps forgets it.'} confirmWord={n.name} steps={off ? ['redeploy containers elsewhere', 'revoke tunnel key', 'forget node'] : ['drain', 'revoke tunnel key', 'forget node']} onDone={() => { notify('warn', `${n.name} removed`); go('settings:nodes') }} />}
      </>}>
      {tab === 'resources' && <NodeResources node={facts} go={go} notify={notify} dense={dense} />}
      {tab === 'agent log' && (
        <Section title="Agent log" meta={off ? 'last lines received · nothing since 21:25' : 'live · newest first'}>
          <LogLines lines={agentLog(n)} live={!off} height={420} search />
        </Section>
      )}
    </Detail>
  )
}

// ── Cluster settings (settings:cluster) ───────────────────────────────

/* The fleet as a graph, from the same NODES the ledger draws. Layer 0 is the
   control plane, layer 1 the workers; the layout is deterministic, so the
   picture the operator saw yesterday is the picture they see today. */
const TOPO_NODES: TopoNode[] = NODES.map((n) => ({
  id: n.name, label: n.name, kind: n.role, state: NODE_STATE[n.status], layer: n.role === 'control plane' ? 0 : 1,
  facts: n.status === 'offline' ? `${n.address} · no heartbeat for ${n.heartbeat.replace(' ago', '')}` : `${n.address} · ${resOf(n.name).vcpu} vCPU · ${containersOf(n.name).length} containers`,
}))
const TOPO_LINKS: TopoLink[] = NODES.filter((n) => n.role !== 'control plane').map((n) => ({
  from: NODES[0].name, to: n.name, kind: n.reach === 'relay' ? 'relay' : 'direct',
  state: n.status === 'offline' ? ('error' as State) : undefined,
}))

export function ClusterPage({ meta, notify, go }: { meta: React.ReactNode; notify: Notify; go?: (v: string) => void }) {
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
    {/* What is joined to what, and how each worker is reached. The list under
        the graph carries the keyboard and the state words; the graph is the
        second view of the same rows. */}
    <Section title="Fleet" meta={`${TOPO_NODES.length} nodes · ${TOPO_LINKS.length} tunnels`}>
      <Topology nodes={TOPO_NODES} links={TOPO_LINKS} label="cluster" height={220}
        onOpen={go ? (n) => go(`node:${n.id}`) : undefined}
        verdict={TOPO_NODES.some((n) => n.state === 'error')
          ? `${TOPO_NODES.find((n) => n.state === 'error')?.label} has not sent a heartbeat for 4 minutes; its relay connection timed out.`
          : 'Every worker is reachable from the control plane.'}
        meta={`heartbeat every 15s · offline after 3 missed · direct needs UDP 51820 both ways, relay needs outbound 443 only`} />
    </Section>
    <EffectLegend />
    </div>
  )
}

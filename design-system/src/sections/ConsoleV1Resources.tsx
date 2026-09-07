// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Bell, Cog, Container, Database, Plus, RefreshCw, Timer } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Callout, ChartFooter, Columns, Drop, EchoDialog, Gauge, KeyValue, Ledger, Lede, Live, MetricGrid, PageState, Phrase,
  ReadoutLive, Section, Segmented, Sparkline, StackedInk, Status, StatusLine, TimeChart,
  fmtBytes, fmtCount, fmtDuration, fmtNum, fmtPct,
  type InkLayer, type KV, type LedgerRow, type Marker, type Series, type State, type TimePoint,
} from '@/components/op'
import type { Notify } from './ConsoleV1Observe'

/**
 * Resources: what a machine and a service are actually using, and what to do
 * about it. Two readers, one shape — a node record answers "is this machine in
 * trouble and which container is doing it", a service's overview answers "what
 * do my replicas cost and where do they run".
 *
 * The rules this screen exists to keep (docs/data-viz.md, RULES.md):
 *  - the page opens with a verdict, never with a number: 91% is not an
 *    instruction, "billing-worker holds 1.4 GiB of it, move it" is;
 *  - the four charts share ONE time axis and ONE cursor, because "cpu at 19:30"
 *    and "memory at 19:30" are the same question asked twice;
 *  - a percentage always arrives with its absolute (91% · 7.3 GiB of 8 GiB);
 *  - a threshold is a dashed line with its own word, and tone appears only
 *    where the series crosses it;
 *  - disk states its projection, because a disk is the one resource whose
 *    future is knowable;
 *  - pressure is attributed: every chart is followed by the containers that
 *    made it, sorted worst first;
 *  - a node with no samples keeps its tiles and says when they were last true.
 *
 * The fixture is deterministic: a seeded PRNG and a clock frozen at
 * 2026-09-06 21:29 UTC, so the same 48 buckets render on every reload and a
 * visual baseline means something. No `Math.random`, no `new Date()`.
 */

// ── The clock, the axis and the seed ───────────────────────────────────

/** The mockup's frozen clock. Every stamp on this screen is derived from it. */
export const NOW = '2026-09-06T21:29:00Z'
const BUCKET_MIN = 30
const BUCKETS = 48
/** The first bucket starts 24h before the last one (21:30 yesterday → 21:00 today). */
const FIRST_MIN = 21 * 60 + 30
/** Bucket labels, `HH:MM`, oldest first. The last one is still filling at 21:29. */
export const AXIS: string[] = Array.from({ length: BUCKETS }, (_, i) => {
  const m = (FIRST_MIN + i * BUCKET_MIN) % 1440
  return `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`
})
const at = (label: string) => AXIS.indexOf(label)
/** Deploys land on the axis of every chart on the page: an axis without them cannot answer "since which deploy". */
export const DEPLOYS: Marker[] = [
  { id: 'dep_31c', x: '19:30', at: '19:31', note: 'billing-worker · 3 replicas' },
  { id: 'dep_91a', x: '20:30', at: '20:34', note: 'api-gateway · main@9bc61c0' },
]

/** mulberry32: the same sequence on every machine, so the fixture is a fact and not a roll of the dice. */
function rng(seed: number) {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6d2b79f5) >>> 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
/** A series: a base level, a little noise, and a shape function that tells the story. */
function series(seed: number, base: number, noise: number, shape: (i: number) => number = () => 0, digits = 1) {
  const r = rng(seed)
  return Array.from({ length: BUCKETS }, (_, i) => {
    const v = base + shape(i) + (r() - 0.5) * noise
    return Number(Math.max(0, v).toFixed(digits))
  })
}
const peak = (xs: number[]) => Math.max(...xs)
const peakLabel = (xs: number[]) => AXIS[xs.indexOf(peak(xs))]
const last = (xs: number[]) => xs[xs.length - 1]

const GIB = 1024 ** 3
const MIB = 1024 ** 2
const GB = 1e9

// ── Containers: one roster, with the numbers on it ─────────────────────

export type ContainerRes = {
  name: string
  project: string
  kind: 'app' | 'static' | 'cron' | 'postgres' | 'redis' | 'clickhouse' | 'system'
  state: State
  node: string
  /** Percent of the node's vCPU. */
  cpu: number
  /** Resident memory, bytes. Binary units: this is what the kernel reports. */
  mem: number
  /** The container's memory limit in bytes, or null when it runs unlimited. */
  limit: number | null
  /** Bytes per second, in + out. */
  net: number
  restarts: number
  /** How long this container has been up, in words. */
  uptime: string
  cpuSpark: number[]
  memSpark: number[]
}

/** A container's own 24h, on the same 48 buckets as its node: one axis for the whole page. */
const SPARK = BUCKETS
const spark = (seed: number, base: number, noise: number, shape: (i: number) => number = () => 0, startAt = 0) => {
  const r = rng(seed)
  return Array.from({ length: BUCKETS }, (_, i) => (i < startAt ? 0 : Number(Math.max(0, base + shape(i) + (r() - 0.5) * noise).toFixed(2))))
}
/** A container that started mid-window and has not stopped growing since: the leak, drawn from where it was born. */
const climb = (startAt: number, from: number, to: number) => (i: number) => from + ((i - startAt) / Math.max(1, BUCKETS - 1 - startAt)) * (to - from)

export const CONTAINERS: ContainerRes[] = [
  // hetzner-1 · the control plane. Steady memory, cpu that spikes while it builds.
  { name: 'acme-storefront-dep_91a-1', project: 'acme-storefront', kind: 'app', state: 'ok', node: 'hetzner-1', cpu: 3.1, mem: 312 * MIB, limit: 512 * MIB, net: 210_000, restarts: 0, uptime: '41m', cpuSpark: spark(11, 3, 3), memSpark: spark(12, 312, 18) },
  { name: 'acme-storefront-dep_91a-2', project: 'acme-storefront', kind: 'app', state: 'ok', node: 'hetzner-1', cpu: 2.8, mem: 298 * MIB, limit: 512 * MIB, net: 198_000, restarts: 0, uptime: '41m', cpuSpark: spark(13, 3, 3), memSpark: spark(14, 298, 18) },
  { name: 'api-gateway-dep_91a-1', project: 'api-gateway', kind: 'app', state: 'ok', node: 'hetzner-1', cpu: 5.2, mem: 300 * MIB, limit: 512 * MIB, net: 640_000, restarts: 0, uptime: '41m', cpuSpark: spark(15, 5, 4), memSpark: spark(16, 300, 14) },
  { name: 'acme-pg', project: 'acme-pg', kind: 'postgres', state: 'ok', node: 'hetzner-1', cpu: 4.4, mem: 1126 * MIB, limit: null, net: 120_000, restarts: 0, uptime: '41d', cpuSpark: spark(17, 4, 3), memSpark: spark(18, 1126, 40) },
  { name: 'sessions-redis', project: 'sessions-redis', kind: 'redis', state: 'warn', node: 'hetzner-1', cpu: 1.2, mem: 241 * MIB, limit: 256 * MIB, net: 88_000, restarts: 1, uptime: '18h 40m', cpuSpark: spark(19, 1.2, 1), memSpark: spark(20, 238, 8) },
  { name: 'temps-preview-gateway', project: 'system', kind: 'system', state: 'ok', node: 'hetzner-1', cpu: 0.4, mem: 48 * MIB, limit: null, net: 41_000, restarts: 0, uptime: '41d', cpuSpark: spark(21, 0.4, 0.6), memSpark: spark(22, 48, 4) },

  // hetzner-2 · the worker the story happens on. billing-worker landed at 19:30 and has not stopped growing.
  { name: 'events-ch', project: 'events-ch', kind: 'clickhouse', state: 'ok', node: 'hetzner-2', cpu: 9.4, mem: 3174 * MIB, limit: null, net: 1_400_000, restarts: 0, uptime: '12d', cpuSpark: spark(31, 9, 5), memSpark: spark(32, 3174, 60) },
  { name: 'billing-worker-dep_31c-1', project: 'billing-worker', kind: 'app', state: 'warn', node: 'hetzner-2', cpu: 14.2, mem: 1434 * MIB, limit: 2048 * MIB, net: 310_000, restarts: 2, uptime: '1h 59m', cpuSpark: spark(33, 12, 6, () => 0, at('19:30')), memSpark: spark(34, 0, 24, climb(at('19:30'), 420, 1434), at('19:30')) },
  { name: 'api-gateway-dep_91a-2', project: 'api-gateway', kind: 'app', state: 'warn', node: 'hetzner-2', cpu: 6.1, mem: 470 * MIB, limit: 512 * MIB, net: 610_000, restarts: 0, uptime: '41m', cpuSpark: spark(35, 6, 4), memSpark: spark(36, 470, 12) },
  { name: 'api-gateway-dep_91a-3', project: 'api-gateway', kind: 'app', state: 'ok', node: 'hetzner-2', cpu: 5.6, mem: 462 * MIB, limit: 512 * MIB, net: 598_000, restarts: 0, uptime: '41m', cpuSpark: spark(37, 5.6, 4), memSpark: spark(38, 462, 12) },
  { name: 'acme-crm-dep_44a-1', project: 'acme-crm', kind: 'app', state: 'ok', node: 'hetzner-2', cpu: 2.2, mem: 180 * MIB, limit: 512 * MIB, net: 96_000, restarts: 0, uptime: '3d 4h', cpuSpark: spark(39, 2.2, 2), memSpark: spark(40, 180, 10) },
  { name: 'acme-crm-dep_44a-2', project: 'acme-crm', kind: 'app', state: 'ok', node: 'hetzner-2', cpu: 2.0, mem: 176 * MIB, limit: 512 * MIB, net: 92_000, restarts: 0, uptime: '3d 4h', cpuSpark: spark(41, 2, 2), memSpark: spark(42, 176, 10) },
  { name: 'docs-dep_12b-1', project: 'docs', kind: 'static', state: 'ok', node: 'hetzner-2', cpu: 0.3, mem: 22 * MIB, limit: 128 * MIB, net: 44_000, restarts: 0, uptime: '9d', cpuSpark: spark(43, 0.3, 0.4), memSpark: spark(44, 22, 2) },

  // hetzner-3 · offline since 21:25. The numbers are the last ones that were true, not zeroes.
  { name: 'billing-worker-dep_31c-2', project: 'billing-worker', kind: 'app', state: 'error', node: 'hetzner-3', cpu: 11.8, mem: 1412 * MIB, limit: 2048 * MIB, net: 288_000, restarts: 1, uptime: '1h 55m', cpuSpark: spark(51, 11, 5, () => 0, at('19:30')), memSpark: spark(52, 0, 24, climb(at('19:30'), 420, 1412), at('19:30')) },
  { name: 'nightly-report', project: 'acme-storefront', kind: 'cron', state: 'error', node: 'hetzner-3', cpu: 0.9, mem: 96 * MIB, limit: 256 * MIB, net: 12_000, restarts: 0, uptime: '17d', cpuSpark: spark(53, 0.9, 1), memSpark: spark(54, 96, 6) },
  { name: 'docs-dep_12b-2', project: 'docs', kind: 'static', state: 'error', node: 'hetzner-3', cpu: 0.2, mem: 22 * MIB, limit: 128 * MIB, net: 9_000, restarts: 0, uptime: '9d', cpuSpark: spark(55, 0.2, 0.3), memSpark: spark(56, 22, 2) },
]

/** The containers on a node. The one roster: the ledger, the record and the cluster graph count the same list. */
export const containersOf = (node: string) => CONTAINERS.filter((c) => c.node === node)
/** The replicas of a service, wherever they run. */
export const replicasOf = (project: string) => CONTAINERS.filter((c) => c.project === project)

// ── Nodes: 24h of samples per machine ──────────────────────────────────

export type NodeRes = {
  /** vCPU count, memory in bytes (binary), disk in bytes (decimal). */
  vcpu: number
  memTotal: number
  diskTotal: number
  /** Link speed in bytes per second, the scale the network gauge is drawn on. */
  link: number
  cpu: number[]
  memPct: number[]
  diskUsed: number[]
  netIn: number[]
  netOut: number[]
  /** Disk growth in bytes per day, measured over the window. Drives the projection. */
  growth: number
  /** Set when the node stopped sending: every figure is the last one that was true, at this time. */
  staleSince?: string
}

/** Builds a memory percentage series, then reads the absolute off the node's total. */
const RES: Record<string, NodeRes> = {
  // The control plane: memory flat, cpu spiky while it builds, disk the only thing moving.
  'hetzner-1': {
    vcpu: 3, memTotal: 4 * GIB, diskTotal: 80 * GB, link: 125e6,
    cpu: series(101, 11, 6, (i) => (i === at('18:30') ? 67 : i === at('19:30') ? 73 : i === at('20:30') ? 73 : i === at('21:00') ? 24 : 0)),
    memPct: series(102, 57, 3),
    diskUsed: series(103, 62.0 * GB, 0.02 * GB, (i) => (i / (BUCKETS - 1)) * 0.4 * GB, 0),
    netIn: series(104, 1_100_000, 300_000, (i) => (i === at('20:30') ? 2_400_000 : 0), 0),
    netOut: series(105, 420_000, 120_000, (i) => (i === at('20:30') ? 1_900_000 : 0), 0),
    growth: 0.4 * GB,
  },
  // The worker the story happens on: billing-worker lands at 19:30 and memory climbs for two hours.
  'hetzner-2': {
    vcpu: 4, memTotal: 8 * GIB, diskTotal: 160 * GB, link: 125e6,
    cpu: series(201, 9, 5, (i) => (i >= at('19:30') ? 9 : 0) + (i === at('20:30') ? 21 : 0)),
    memPct: series(202, 38, 2.5, (i) => (i < at('19:30') ? 0 : ((i - at('19:30')) / (BUCKETS - 1 - at('19:30'))) * 53)),
    diskUsed: series(203, 35.2 * GB, 0.05 * GB, (i) => (i / (BUCKETS - 1)) * 0.1 * GB, 0),
    netIn: series(204, 900_000, 250_000, (i) => (i === at('20:30') ? 8_600_000 : i === at('19:30') ? 3_100_000 : 0), 0),
    netOut: series(205, 340_000, 90_000, (i) => (i === at('20:30') ? 2_700_000 : 0), 0),
    growth: 0.1 * GB,
  },
  // Offline since 21:25: the samples stop where the heartbeat stopped.
  'hetzner-3': {
    vcpu: 2, memTotal: 4 * GIB, diskTotal: 40 * GB, link: 125e6,
    cpu: series(301, 12, 5),
    memPct: series(302, 44, 3),
    diskUsed: series(303, 12.4 * GB, 0.02 * GB, () => 0, 0),
    netIn: series(304, 280_000, 90_000, () => 0, 0),
    netOut: series(305, 120_000, 40_000, () => 0, 0),
    growth: 0.05 * GB,
    staleSince: '21:25',
  },
}
export const resOf = (node: string): NodeRes => RES[node] ?? RES['hetzner-1']

/** The four facts a machine is judged on, in one place, so the tiles, the lede and the verdict cannot disagree. */
export function readingsOf(node: string) {
  const r = resOf(node)
  const memPct = last(r.memPct)
  const diskUsed = last(r.diskUsed)
  const diskPct = (diskUsed / r.diskTotal) * 100
  const free = r.diskTotal - diskUsed
  const daysToFull = r.growth > 0 ? free / r.growth : Infinity
  const toErrorLine = r.diskTotal * 0.9 - diskUsed
  const daysToLine = r.growth > 0 ? toErrorLine / r.growth : Infinity
  // The arrays keep their names; the current reading takes a `Now` so a plot and
  // a tile can never be handed the same word and mean different things.
  return {
    ...r,
    cpuNow: last(r.cpu), cpuPeak: peak(r.cpu), cpuPeakAt: peakLabel(r.cpu),
    memNow: memPct, memUsed: (memPct / 100) * r.memTotal, memPeak: peak(r.memPct), memPeakAt: peakLabel(r.memPct),
    diskNow: diskUsed, diskPct, free, daysToFull, daysToLine,
    netInNow: last(r.netIn), netOutNow: last(r.netOut), netPeak: peak(r.netIn.map((v, i) => v + r.netOut[i])),
    stale: r.staleSince,
  }
}

/** Sparkline series for the nodes ledger: four shapes per row, one per resource. */
export function sparksOf(node: string) {
  const r = resOf(node)
  const step = Math.max(1, Math.floor(BUCKETS / SPARK))
  const thin = (xs: number[]) => xs.filter((_, i) => i % step === 0)
  return { cpu: thin(r.cpu), mem: thin(r.memPct), disk: thin(r.diskUsed.map((v) => (v / r.diskTotal) * 100)), net: thin(r.netIn.map((v, i) => v + r.netOut[i])) }
}

/** Worst-first: an offline machine, then the highest of the three pressures. Used by the ledger's default order. */
export function pressureRank(node: string, offline: boolean) {
  if (offline) return -1
  const rd = readingsOf(node)
  return -Math.max(rd.cpuNow, rd.memNow, rd.diskPct)
}

// ── The thresholds, once ───────────────────────────────────────────────

type Threshold = { at: number; state: 'warn' | 'error'; label: string }
const CPU_LINES: Threshold[] = [{ at: 80, state: 'warn', label: 'busy' }, { at: 95, state: 'error', label: 'saturated' }]
const MEM_LINES: Threshold[] = [{ at: 85, state: 'warn', label: 'tight' }, { at: 95, state: 'error', label: 'oom risk' }]
const DISK_LINES: Threshold[] = [{ at: 80, state: 'warn', label: 'tight' }, { at: 90, state: 'error', label: 'writes stop' }]
const crossed = (value: number, lines: Threshold[]): 'ok' | 'warn' | 'error' => {
  const hit = [...lines].sort((a, b) => b.at - a.at).find((t) => value >= t.at)
  return hit ? hit.state : 'ok'
}

// ── The shared axis: one cursor over four charts ───────────────────────

/**
 * `TimeChart` owns its own hover state and has no `cursor`/`onHover` props, so
 * four charts cannot be made to read the same bucket from the package alone
 * (see the package gaps in docs/handoff.additions.resources.md). This is the
 * local wrapper: the group holds one bucket index, every pane reports the
 * pointer's position on the shared axis, every chart's readout is formatted
 * from that index, and a dotted ink cursor is drawn over all four at the same
 * fraction of the plot. `←` `→` on the cursor control move it without a
 * pointer, and the value is announced in a live region.
 */
const PLOT_INSET = 42 // px of y-axis gutter TimeChart reserves on the left

function useSharedCursor(length: number) {
  const [i, setI] = useState<number | null>(null)
  const fromPointer = useCallback((el: HTMLElement, clientX: number) => {
    const r = el.getBoundingClientRect()
    const plot = r.width - PLOT_INSET
    if (plot <= 0) return
    const f = (clientX - r.left - PLOT_INSET) / plot
    setI(Math.max(0, Math.min(length - 1, Math.round(f * (length - 1)))))
  }, [length])
  const move = useCallback((d: number) => setI((cur) => Math.max(0, Math.min(length - 1, (cur ?? length - 1) + d))), [length])
  return { i, setI, fromPointer, move }
}

function AxisPane({ cursor, length, onPointer, onLeave, children }: { cursor: number | null; length: number; onPointer: (el: HTMLElement, x: number) => void; onLeave: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null)
  const frac = cursor === null ? null : cursor / Math.max(1, length - 1)
  return (
    <div ref={ref} className="relative min-w-0"
      onPointerMove={(e) => ref.current && onPointer(ref.current, e.clientX)}
      onPointerLeave={onLeave}>
      {children}
      {frac !== null && (
        <span aria-hidden className="pointer-events-none absolute inset-y-6 w-px border-l border-dashed border-foreground/50"
          style={{ insetInlineStart: `calc(${PLOT_INSET}px + (100% - ${PLOT_INSET}px) * ${frac})` }} />
      )}
    </div>
  )
}

// ── Bits every chart on the page shares ────────────────────────────────

const points = (xs: number[], key: string): TimePoint[] => xs.map((v, i) => ({ t: AXIS[i], [key]: v }))
/**
 * Tone belongs to the stretch that crossed the line, not to the whole day. The
 * series stays ink; a second series carries only the buckets at or above the
 * threshold, drawn on top, in the threshold's tone, and out of the table view
 * because it is the same numbers as the line it marks.
 */
function overed(xs: number[], key: string, warnAt: number, errorAt: number, word: string, unit: string) {
  const data: TimePoint[] = xs.map((v, i) => (v >= warnAt ? { t: AXIS[i], [key]: v, over: v } : { t: AXIS[i], [key]: v }))
  const buckets = xs.filter((v) => v >= warnAt).length
  const hitError = xs.some((v) => v >= errorAt)
  const extra: Series[] = buckets
    ? [{ key: 'over', name: `above ${word} ${fmtNum(hitError ? errorAt : warnAt)}${unit}`, state: hitError ? 'error' : 'warn', stroke: 'solid', weight: 'regular', top: true, inTable: false }]
    : []
  return { data, buckets, extra, note: buckets ? `· ◐ above ${word} for ${fmtCount(buckets, 'bucket')} (${fmtDuration(buckets * BUCKET_MIN * 60_000)})` : null }
}
const zip = (a: number[], b: number[], ka: string, kb: string): TimePoint[] => a.map((v, i) => ({ t: AXIS[i], [ka]: v, [kb]: b[i] }))
const bytesPerSec = (n: number) => `${fmtBytes(n)}/s`
/** "full in 44 d", "full in 3 months" — a projection is stated in the unit an operator plans in. */
const inDays = (d: number) =>
  !Number.isFinite(d) ? 'not growing'
  : d < 1 ? 'today'
  : d < 90 ? `${fmtNum(Math.round(d))} d`
  : d < 730 ? `${fmtNum(Math.round(d / 30))} months`
  : `${fmtNum(Math.round(d / 365))} years`
/** The date a projection lands on, from the frozen clock. */
const dateIn = (days: number) => {
  if (!Number.isFinite(days)) return '–'
  const d = new Date(Date.parse(NOW) + days * 86_400_000)
  const year = d.getUTCFullYear()
  const thisYear = new Date(Date.parse(NOW)).getUTCFullYear()
  return `${d.toLocaleString('en', { month: 'short', timeZone: 'UTC' })} ${d.getUTCDate()}${year === thisYear ? '' : ` ${year}`}`
}
/** Node samples are kept for a week; telemetry's own 30d horizon is a different shelf and says so in the footer. */
const RETENTION = { days: 7, label: '7d' }
const RANGES = [['1h', '1h'], ['24h', '24h'], ['7d', '7d'], ['30d', '30d']] as const
type RangeKey = (typeof RANGES)[number][0]

/**
 * The range strip: 1h · 24h · 7d · 30d, with everything past the sample
 * horizon struck through rather than hidden. `RangePicker` takes a plan's
 * retention and renders the same strike, but it also owns a custom-window
 * popover this screen does not want on a phone; the strip is the same rule in
 * one line. (Package gap: `RangePicker` cannot be asked for the strip alone.)
 */
function Ranges({ value, onChange, onGated }: { value: RangeKey; onChange: (v: RangeKey) => void; onGated: (v: RangeKey) => void }) {
  const days: Record<RangeKey, number> = { '1h': 0.04, '24h': 1, '7d': 7, '30d': 30 }
  return (
    <div className="op-scroll-x flex min-w-0 max-w-full border text-xs">
      {RANGES.map(([v, label], i) => {
        const gated = days[v] > RETENTION.days
        return (
          <button key={v} type="button" aria-pressed={value === v} onClick={() => (gated ? onGated(v) : onChange(v))}
            title={gated ? `node samples are kept ${RETENTION.label}` : undefined}
            className={`h-7 shrink-0 whitespace-nowrap px-2.5 font-mono ${i > 0 ? 'border-l' : ''} ${value === v ? 'bg-muted' : 'hover:bg-muted'} ${gated ? 'text-muted-foreground line-through' : ''}`}>{label}</button>
        )
      })}
    </div>
  )
}

/**
 * Live, and honest about it: every 30s, and it stops the moment the reader
 * scrolls away from the top of the page, because a number that moves under
 * somebody reading it is worse than a number that is a minute old. It says
 * "paused" and the same control resumes.
 */
function useLive() {
  const [live, setLive] = useState(true)
  const [autoPaused, setAutoPaused] = useState(false)
  const paused = !live || autoPaused
  const toggle = useCallback(() => { setLive((on) => (paused ? true : !on)); setAutoPaused(false) }, [paused])
  useEffect(() => {
    if (!live) return
    const onScroll = () => { if (window.scrollY > 120) setAutoPaused(true) }
    window.addEventListener('scroll', onScroll, { passive: true })
    return () => window.removeEventListener('scroll', onScroll)
  }, [live])
  // `Live` draws a `space` badge, and a badge with no handler is a lie.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName
      if (e.key !== ' ' || tag === 'INPUT' || tag === 'TEXTAREA' || e.metaKey || e.ctrlKey) return
      e.preventDefault()
      toggle()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [toggle])
  return { paused, toggle }
}

// ── The verdict and the lede a node record opens with ──────────────────

export type NodeFacts = {
  name: string
  role: 'control plane' | 'worker'
  reach: 'local' | 'direct' | 'relay'
  arch: string
  offline: boolean
  draining: boolean
  heartbeat: string
  agent: string
}

/** The verdict: the action, never a bare number. Healthy still says what it proved. */
export function nodeVerdict(n: NodeFacts, go: (v: string) => void): ReactNode {
  const rd = readingsOf(n.name)
  const containers = containersOf(n.name)
  if (n.offline) {
    return (
      <StatusLine state="error">
        No heartbeat for 4 minutes; the last one was at {rd.stale}. The relay connection timed out, so the {fmtCount(containers.length, 'container')} on it are unreachable and billing-worker answers 502.
        Check the agent on the machine (<span className="font-mono">systemctl status temps-agent</span>), or <Phrase onClick={() => go('settings:cluster')}>drain it</Phrase> to move the work.
      </StatusLine>
    )
  }
  if (n.draining) return <StatusLine state="warn">Draining: containers are moving to other nodes. New deploys skip this node until you undrain it.</StatusLine>
  const hog = [...containers].sort((a, b) => b.mem - a.mem)[0]
  if (rd.memNow >= MEM_LINES[0].at) {
    return (
      <StatusLine state="warn">
        Memory is at {fmtPct(rd.memNow, { digits: 0 })} of {fmtBytes(rd.memTotal, { binary: true })} and has been climbing for two hours, since <Phrase onClick={() => go(`deploy:${DEPLOYS[0].id}`)}>{DEPLOYS[0].id}</Phrase>.
        {' '}<Phrase onClick={() => go(hog.project)}>{hog.name}</Phrase> holds {fmtBytes(hog.mem, { binary: true })} of it and has restarted {fmtCount(hog.restarts, 'time')}.
        Move it to another node, or <Phrase onClick={() => go('settings:cluster')}>add a node</Phrase>.
      </StatusLine>
    )
  }
  return (
    <StatusLine state="ok">
      Nothing to do: cpu, memory, disk and network are all under their warn lines, and the busiest is disk at {fmtPct(rd.diskPct, { digits: 0 })}.
      At {fmtBytes(rd.growth)} a day the disk is full in {inDays(rd.daysToFull)}.
    </StatusLine>
  )
}

/** Six facts: the four resources, what is running, and whether the machine is still talking. */
export function nodeLedeFacts(n: NodeFacts): KV[] {
  const rd = readingsOf(n.name)
  const off = n.offline
  const grey = (v: ReactNode) => (off ? <span className="text-muted-foreground">{v}</span> : v)
  return [
    { k: 'cpu', v: grey(<>{fmtPct(rd.cpuNow, { digits: 0 })} <span className="text-muted-foreground">· peak {fmtPct(rd.cpuPeak, { digits: 0 })} at {rd.cpuPeakAt}</span></>), state: off ? undefined : crossed(rd.cpuNow, CPU_LINES) === 'ok' ? undefined : crossed(rd.cpuNow, CPU_LINES) },
    { k: 'memory', v: grey(<>{fmtBytes(rd.memUsed, { binary: true })} <span className="text-muted-foreground">of {fmtBytes(rd.memTotal, { binary: true })} · {fmtPct(rd.memNow, { digits: 0 })}</span></>), state: off ? undefined : crossed(rd.memNow, MEM_LINES) === 'ok' ? undefined : crossed(rd.memNow, MEM_LINES) },
    { k: 'disk free', v: grey(<>{fmtBytes(rd.free)} <span className="text-muted-foreground">· full in {inDays(rd.daysToFull)}</span></>) },
    { k: 'network', v: grey(<>{bytesPerSec(rd.netInNow)} in <span className="text-muted-foreground">· {bytesPerSec(rd.netOutNow)} out</span></>) },
    { k: 'containers', v: grey(off ? `${containersOf(n.name).length} unreachable` : fmtCount(containersOf(n.name).length, 'running', 'running')), state: off ? 'error' : undefined },
    { k: 'heartbeat', v: <>{n.heartbeat} <span className="text-muted-foreground">· agent {n.agent}</span></>, state: off ? 'error' : undefined },
  ]
}

export function NodeLede({ node }: { node: NodeFacts }) {
  const rd = readingsOf(node.name)
  const word = node.offline ? 'offline' : node.draining ? 'draining' : 'online'
  const state: State = node.offline ? 'error' : node.draining ? 'warn' : crossed(rd.memNow, MEM_LINES) === 'ok' ? 'ok' : 'warn'
  return (
    <Lede state={state} word={word} facts={nodeLedeFacts(node)}>{rd.vcpu} vCPU · {fmtBytes(rd.memTotal, { binary: true })} · {fmtBytes(rd.diskTotal)} disk{node.offline ? ` · last sample ${rd.stale}` : ''}</Lede>
  )
}

// ── The four charts, on one axis and one cursor ────────────────────────

type Pane = {
  key: string
  /** The unit, said once, in the pane's header. */
  header: ReactNode
  /** What the chart reads at bucket `i`. Every pane answers for the same bucket. */
  read: (i: number) => string
  /** The chart itself, given the readout formatter the shared cursor drives. */
  render: (readout: (p: TimePoint) => string) => ReactNode
  footer?: ReactNode
  action?: ReactNode
}

/**
 * Four charts, one axis, one cursor. Hovering 14:20 on any of them reads 14:20
 * on all four, because "what was cpu doing when memory climbed" is one
 * question. The cursor control under the strip is the keyboard's way in:
 * `←` `→` walk the buckets, `esc` clears, and the bucket is announced.
 */
function ChartStack({ panes, length }: { panes: Pane[]; length: number }) {
  const { i, setI, fromPointer, move } = useSharedCursor(length)
  const idx = i ?? length - 1
  return (
    <div className="min-w-0 space-y-4">
      <div
        role="group" tabIndex={0}
        aria-label={`shared cursor over ${length} buckets · left and right arrows move it, escape clears it`}
        onKeyDown={(e) => {
          if (e.key === 'ArrowRight') { e.preventDefault(); move(1) }
          else if (e.key === 'ArrowLeft') { e.preventDefault(); move(-1) }
          else if (e.key === 'Escape') setI(null)
        }}
        className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1 border px-3 py-2 font-mono text-[11px] outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
        <span className="op-label">cursor</span>
        <span className="tabular-nums">{AXIS[idx]}</span>
        <span className="text-muted-foreground">{i === null ? 'latest bucket · hover any chart, or ← → to move' : 'all four charts read this bucket · esc to clear'}</span>
      </div>
      <ReadoutLive text={i === null ? '' : `${AXIS[idx]} · ${panes.map((p) => p.read(idx)).join(' · ')}`} />
      {panes.map((p) => (
        <div key={p.key} className="min-w-0 border bg-background p-3">
          <div className="mb-1 flex min-w-0 flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
            <span className="op-label">{p.header}</span>
            {p.action}
          </div>
          <AxisPane cursor={i} length={length} onPointer={fromPointer} onLeave={() => setI(null)}>
            {/* The bucket rides every readout: `TimeChart` owns the word beside it ("hover"/"latest") and cannot be told about a cursor it does not own. */}
            {p.render(() => `${AXIS[idx]} · ${p.read(idx)}`)}
          </AxisPane>
          {p.footer && <ChartFooter>{p.footer}</ChartFooter>}
        </div>
      ))}
    </div>
  )
}

// ── The containers under the charts ────────────────────────────────────

const KIND_ICON: Partial<Record<ContainerRes['kind'], ReactNode>> = {
  app: <Container aria-hidden />, static: <Container aria-hidden />, cron: <Timer aria-hidden />,
  postgres: <Database aria-hidden />, redis: <Database aria-hidden />, clickhouse: <Database aria-hidden />, system: <Cog aria-hidden />,
}
const kindIcon = (k: ContainerRes['kind']) => KIND_ICON[k] ?? <Container aria-hidden />
/** Worst first: the state, then how close the container is to its own limit. */
const pressureOf = (c: ContainerRes) => (c.limit ? c.mem / c.limit : c.mem / (8 * GIB))
const RANK: Partial<Record<State, number>> = { error: 0, warn: 1, running: 2, ok: 3, idle: 4 }
const rankOf = (s: State) => RANK[s] ?? 5

/**
 * Who is using it. One `Ledger`, sorted pressure-first, `⏎` opening the
 * service the container belongs to: a chart that says memory is at 91% and
 * does not say which container holds it has told the reader nothing they can
 * act on.
 */
function ContainerLedger({ rows: cs, dense, go, meta, showNode }: { rows: ContainerRes[]; dense: boolean; go: (v: string) => void; meta: ReactNode; showNode?: boolean }) {
  const [q, setQ] = useState('')
  const shown = useMemo(
    () => cs.filter((c) => c.name.toLowerCase().includes(q.trim().toLowerCase()) || c.project.toLowerCase().includes(q.trim().toLowerCase()))
      .sort((a, b) => rankOf(a.state) - rankOf(b.state) || pressureOf(b) - pressureOf(a)),
    [cs, q])
  const limitCell = (c: ContainerRes) => {
    const share = c.limit ? (c.mem / c.limit) * 100 : null
    const state = share === null ? undefined : share >= 95 ? 'error' : share >= 90 ? 'warn' : undefined
    return (
      <span className={`font-mono ${state === 'error' ? 'text-destructive' : state === 'warn' ? 'text-warning' : ''}`}>
        {c.limit ? fmtNum(Math.round(c.mem / MIB)) : fmtBytes(c.mem, { binary: true })}
        <span className="text-muted-foreground"> {c.limit ? `of ${fmtNum(Math.round(c.limit / MIB))} MiB · ${fmtPct(share ?? 0, { digits: 0 })}` : 'no limit'}</span>
      </span>
    )
  }
  /*
   * The row is the control, so nothing inside it may be one: a `role="button"`
   * row containing a button is `nested-interactive`, and a keyboard reader
   * lands on a row whose Enter and whose child's Enter do different things.
   * The destination therefore follows the reader instead of being duplicated —
   * on a machine the row opens the service the container belongs to, and on a
   * service the row opens the node the replica runs on, which is the only
   * navigation this facet exists to offer (the project is the page you are
   * standing on). The node stays a fact in its cell, not a second control.
   */
  const rows: LedgerRow[] = shown.map((c) => ({
    id: c.name, state: c.state, icon: kindIcon(c.kind),
    onOpen: () => go(showNode ? `node:${c.node}` : c.project === 'system' ? 'settings:nodes' : c.project),
    sort: { name: c.name, cpu: c.cpu, mem: c.mem, restarts: c.restarts, node: c.node },
    mobile: <><span className="block truncate font-mono">{c.name}</span><span className="block truncate text-[11px] text-muted-foreground">{limitCell(c)}{showNode && <> · {c.node}</>}</span></>,
    cells: [
      <span className="min-w-0 truncate font-mono">{c.name}</span>,
      ...(showNode ? [<span className="font-mono text-muted-foreground">{c.node}</span>] : []),
      <span className="font-mono">{fmtPct(c.cpu, { digits: 1 })}</span>,
      limitCell(c),
      <span className="font-mono text-muted-foreground">{bytesPerSec(c.net)}</span>,
      <span className={`font-mono ${c.restarts > 0 ? 'text-warning' : 'text-muted-foreground'}`}>{fmtNum(c.restarts)}</span>,
      <span className="font-mono text-muted-foreground">{c.uptime}</span>,
      <span className="block w-full text-muted-foreground"><Sparkline points={c.cpuSpark} height={16} /></span>,
      <span className="block w-full text-muted-foreground"><Sparkline points={c.memSpark} height={16} state={c.limit && c.mem / c.limit >= 0.9 ? 'warn' : undefined} /></span>,
    ],
  }))
  return (
    <Ledger status={null} dense={dense} meta={meta}
      columns={[{ label: 'container', key: 'name' }, ...(showNode ? [{ label: 'node', key: 'node' }] : []), { label: 'cpu', key: 'cpu', numeric: true }, { label: 'memory of limit', key: 'mem', numeric: true }, 'network', { label: 'restarts', key: 'restarts', numeric: true }, 'uptime', 'cpu · 24h', 'memory · 24h']}
      grid={`minmax(8rem,1.4fr) ${showNode ? 'minmax(5.5rem,max-content) ' : ''}minmax(3.5rem,max-content) minmax(9.5rem,max-content) minmax(4.5rem,max-content) minmax(3.5rem,max-content) minmax(4rem,max-content) minmax(3.5rem,0.5fr) minmax(3.5rem,0.5fr)`}
      rows={rows} total={cs.length} filter={q} onFilter={setQ} placeholder="filter containers"
      hint={`pressure first: failing, then closest to its own limit · ⏎ opens the ${showNode ? 'node it runs on' : 'service'}`}
      footer={<span>memory is what the kernel reports (binary units) · a container with no limit is capped by the node</span>} />
  )
}

// ── The node record's body ─────────────────────────────────────────────

/**
 * `node:<name>`, the machine view. The record recipe with monitoring in it:
 * verdict, lede, then the four charts on one axis with the containers that
 * made them underneath, and the reference facts and the actions in the aside.
 *
 * An offline machine keeps every tile and every plot and greys them, with the
 * time they were last true beside each: an empty page is indistinguishable
 * from a healthy one, and this machine is neither.
 */
export function NodeResources({ node, go, notify, dense }: { node: NodeFacts; go: (v: string) => void; notify: Notify; dense: boolean }) {
  const rd = readingsOf(node.name)
  const cs = containersOf(node.name)
  const off = node.offline
  const [range, setRange] = useState<RangeKey>('24h')
  const [memBy, setMemBy] = useState<'total' | 'container'>('total')
  const [alerting, setAlerting] = useState(false)
  const { paused, toggle } = useLive()
  const asideBtn = useRef<HTMLButtonElement>(null)
  const [details, setDetails] = useState(false)

  const dg = (v: string) => (off ? `${v} · last true ${rd.stale}` : v)

  // Memory by container: the three biggest, then an honest remainder. Four layers is the ceiling.
  const top = [...cs].sort((a, b) => b.mem - a.mem).slice(0, 3)
  const stack: TimePoint[] = AXIS.map((t, i) => {
    const totalMiB = ((rd.memPct[i] ?? 0) / 100) * rd.memTotal / MIB
    const named = top.reduce((a, c) => a + (c.memSpark[i] ?? 0), 0)
    const row: TimePoint = { t, other: Number(Math.max(0, totalMiB - named).toFixed(0)) }
    top.forEach((c) => { row[c.name] = c.memSpark[i] ?? 0 })
    return row
  })
  const layers: InkLayer[] = [
    ...top.map((c, n) => ({ key: c.name, name: c.name.replace(/-dep_[a-z0-9]+-\d+$/, ''), fill: (['solid', 'hatch', 'dot'] as const)[n], state: c.state === 'warn' ? ('warn' as const) : undefined })),
    { key: 'other', name: 'everything else', fill: 'cross' as const },
  ]

  const cpuOver = overed(rd.cpu, 'cpu', CPU_LINES[0].at, CPU_LINES[1].at, CPU_LINES[0].label, '%')
  const memOver = overed(rd.memPct, 'mem', MEM_LINES[0].at, MEM_LINES[1].at, MEM_LINES[0].label, '%')
  // Disk is drawn as a share of the volume, not in GB: the axis is then bounded
  // 0–100 and its two threshold lines are always on it, however empty the disk
  // is. The absolutes ride the readout, the header and the footer.
  const diskPctS = rd.diskUsed.map((v) => Number(((v / rd.diskTotal) * 100).toFixed(2)))
  const trendPct = rd.diskUsed.map((_, i) => Number((((rd.diskUsed[0] + (i / (BUCKETS - 1)) * rd.growth) / rd.diskTotal) * 100).toFixed(2)))
  const diskOver = overed(diskPctS, 'used', DISK_LINES[0].at, DISK_LINES[1].at, DISK_LINES[0].label, '%')
  const netInK = rd.netIn.map((v) => Math.round(v / 1000))
  const netOutK = rd.netOut.map((v) => Math.round(v / 1000))

  const panes: Pane[] = [
    {
      key: 'cpu', header: <>cpu · % of {rd.vcpu} vCPU</>,
      read: (i) => `cpu ${fmtPct(rd.cpu[i], { digits: 0 })}`,
      render: (readout) => (
        <TimeChart data={cpuOver.data} series={[{ key: 'cpu', name: 'cpu' }, ...cpuOver.extra]}
          unit="%" height={132} yTicks={[0, 50, 100]} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          thresholds={CPU_LINES.map((t) => ({ y: t.at, label: t.label, state: t.state }))}
          title={`cpu on ${node.name}`} range={`last ${range}`}
          verdict={off ? `No samples since ${rd.stale}; these are the last values that were true.` : `Between ${fmtPct(Math.min(...rd.cpu), { digits: 0 })} and ${fmtPct(rd.cpuPeak, { digits: 0 })}, peaking at ${rd.cpuPeakAt} while it built.`} />
      ),
      footer: <><span>cpu / 30 min · last {range}</span><span>· retention {RETENTION.label}</span><span>· ┆ deploy</span><span>· ▨ current bucket partial</span>{cpuOver.note && <span className="text-warning">{cpuOver.note}</span>}</>,
    },
    {
      key: 'mem', header: <>memory · {memBy === 'total' ? '% of' : 'MiB of'} {fmtBytes(rd.memTotal, { binary: true })}</>,
      read: (i) => `memory ${fmtPct(rd.memPct[i], { digits: 0 })} (${fmtBytes((rd.memPct[i] / 100) * rd.memTotal, { binary: true })})`,
      action: <Segmented options={[['total', 'total'], ['container', 'by container']] as const} value={memBy} onChange={setMemBy} className="h-6 [&>button]:h-6" />,
      render: (readout) => memBy === 'total' ? (
        <TimeChart data={memOver.data} series={[{ key: 'mem', name: 'memory' }, ...memOver.extra]}
          unit="%" height={132} yTicks={[0, 50, 100]} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          thresholds={MEM_LINES.map((t) => ({ y: t.at, label: t.label, state: t.state }))}
          title={`memory on ${node.name}`} range={`last ${range}`}
          verdict={off ? `No samples since ${rd.stale}; these are the last values that were true.`
            : !memOver.buckets ? `Flat at ${fmtPct(rd.memNow, { digits: 0 })} of ${fmtBytes(rd.memTotal, { binary: true })}.`
            : `Flat at 38% until ${DEPLOYS[0].x}, then climbing to ${fmtPct(rd.memNow, { digits: 0 })} after ${DEPLOYS[0].id}.`} />
      ) : (
        <StackedInk data={stack} layers={layers} unit="MiB" height={132} xInterval={8} partial
          title={`memory by container on ${node.name}`} range={`last ${range}`}
          verdict={`${layers[0].name} is the largest, and the layer that grew after ${DEPLOYS[0].id} is ${top.find((c) => c.state === 'warn')?.name ?? top[0].name}.`} />
      ),
      footer: (
        <>
          <span>memory / 30 min · last {range}</span><span>· retention {RETENTION.label}</span>{memOver.note && <span className="text-warning">{memOver.note}</span>}
          <span className="ms-auto">
            {alerting
              ? <span className="text-warning">◐ alerting when memory &gt; 90% for 10m · <button type="button" className="underline underline-offset-4" onClick={() => { setAlerting(false); notify('ok', 'monitor removed', `${node.name} · memory`) }}>undo</button></span>
              : <button type="button" className="inline-flex items-center gap-1.5 underline underline-offset-4 hover:text-foreground" onClick={() => { setAlerting(true); notify('ok', 'monitor created', `${node.name} · memory > 90% for 10m`) }}><Bell className="size-3.5" aria-hidden /> alert when memory &gt; 90% for 10m</button>}
          </span>
        </>
      ),
    },
    {
      key: 'disk', header: <>disk · % of {fmtBytes(rd.diskTotal)}</>,
      read: (i) => `disk ${fmtPct((rd.diskUsed[i] / rd.diskTotal) * 100, { digits: 0 })} · ${fmtBytes(rd.diskUsed[i])} of ${fmtBytes(rd.diskTotal)}`,
      render: (readout) => (
        <TimeChart data={diskOver.data.map((p, i) => ({ ...p, trend: trendPct[i] }))}
          series={[{ key: 'used', name: 'used' }, { key: 'trend', name: `projection · ${fmtBytes(rd.growth)}/day`, stroke: 'dashed', weight: 'thin' }, ...diskOver.extra]}
          unit="%" height={132} yTicks={[0, 50, 100]} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          thresholds={DISK_LINES.map((t) => ({ y: t.at, label: `${t.label} ${t.at}%`, state: t.state }))}
          title={`disk on ${node.name}`} range={`last ${range}`}
          verdict={`${fmtPct(rd.diskPct, { digits: 0 })} used, growing ${fmtBytes(rd.growth)} a day: it reaches the 90% line in ${inDays(rd.daysToLine)}, on ${dateIn(rd.daysToLine)}.`} />
      ),
      footer: <><span>disk / 30 min · last {range}</span><span>· retention {RETENTION.label}</span><span>· {fmtBytes(rd.free)} free · at {fmtBytes(rd.growth)}/day it hits the 90% line on {dateIn(rd.daysToLine)} and is full on {dateIn(rd.daysToFull)}</span>{diskOver.note && <span className="text-warning">{diskOver.note}</span>}</>,
    },
    {
      key: 'net', header: <>network · kB/s on a {fmtBytes(rd.link)}/s link</>,
      read: (i) => `network ${bytesPerSec(rd.netIn[i])} in · ${bytesPerSec(rd.netOut[i])} out`,
      render: (readout) => (
        <TimeChart data={zip(netInK, netOutK, 'in', 'out')} series={[{ key: 'in', name: 'in' }, { key: 'out', name: 'out', stroke: 'dashed', weight: 'thin' }]}
          unit="kB/s" height={132} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          title={`network on ${node.name}`} range={`last ${range}`}
          verdict={`In and out both flat until ${DEPLOYS[1].id} at ${DEPLOYS[1].at}, which pulled the image and burst to ${bytesPerSec(Math.max(...rd.netIn))}.`} />
      ),
      footer: <><span>bytes / 30 min · last {range}</span><span>· retention {RETENTION.label}</span><span>· ┆ deploy</span></>,
    },
  ]

  const aside = (
    <>
      <Section title="Pressure" meta={off ? `last known · ${rd.stale}` : 'now · peak in the window'}>
        <div className={off ? 'text-muted-foreground' : undefined}>
          <MetricGrid cols={2}>
            <Gauge label="cpu" value={Number(rd.cpuNow.toFixed(1))} of={dg(`of ${rd.vcpu} vCPU`)} peak={rd.cpuPeak} peakLabel={`at ${rd.cpuPeakAt}`} thresholds={CPU_LINES} />
            <Gauge label="memory" value={Number(rd.memNow.toFixed(1))} of={dg(`${fmtBytes(rd.memUsed, { binary: true })} of ${fmtBytes(rd.memTotal, { binary: true })}`)} peak={rd.memPeak} peakLabel={`at ${rd.memPeakAt}`} thresholds={MEM_LINES} />
            <Gauge label="disk" value={Number(rd.diskPct.toFixed(1))} of={dg(`${fmtBytes(rd.diskNow)} of ${fmtBytes(rd.diskTotal)}`)} thresholds={DISK_LINES} />
            <Gauge label="network" value={Number(((rd.netInNow + rd.netOutNow) / 1e6).toFixed(1))} max={Math.round(rd.link / 1e6)} unit=" MB/s" of={dg(`of ${fmtBytes(rd.link)}/s`)} peak={Number((rd.netPeak / 1e6).toFixed(1))} peakLabel={`at ${DEPLOYS[1].at}`} thresholds={[{ at: Math.round((rd.link * 0.8) / 1e6), state: 'warn', label: 'link busy' }]} />
          </MetricGrid>
        </div>
      </Section>
      <Section title="What is using it" meta="top three by memory">
        <ol className="op-rows border bg-background text-xs">
          {[...cs].sort((a, b) => b.mem - a.mem).slice(0, 3).map((c) => (
            <li key={c.name} className="flex items-center justify-between gap-3 px-3 py-2">
              <button type="button" onClick={() => go(c.project === 'system' ? 'settings:nodes' : c.project)} className="min-w-0 truncate text-start font-mono underline-offset-4 hover:underline"><Status state={c.state} label={c.name} /></button>
              <span className="shrink-0 font-mono text-muted-foreground">{fmtBytes(c.mem, { binary: true })}</span>
            </li>
          ))}
        </ol>
      </Section>
      <Section title="Machine">
        <KeyValue compact rows={[
          { k: 'arch', v: node.arch, mono: true },
          { k: 'kernel', v: node.arch === 'arm64' ? '6.8.0-51-generic (aarch64)' : '6.8.0-51-generic', mono: true },
          { k: 'docker', v: '27.3.1 · overlay2', mono: true },
          { k: 'disk device', v: '/dev/sda1 · ext4', mono: true },
        ]} />
      </Section>
      <Section title="Actions" meta="drain is typed, restart is confirmed">
        <div className="flex flex-wrap gap-2">
          {node.role === 'worker' && (
            <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs">drain node</Button>}
              echo={`$ temps node drain ${node.name}`} title={`Drain ${node.name}`}
              description={`Its ${fmtCount(cs.length, 'container')} are redeployed on other nodes, one at a time. New deploys skip ${node.name} until you undrain it. Nothing is deleted.`}
              confirmWord={node.name} steps={['mark unschedulable', 'redeploy containers elsewhere', 'wait for health']}
              onDone={() => notify('ok', `${node.name} drained`, `${cs.length} containers moved`)} />
          )}
          <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><RefreshCw /> restart agent</Button>}
            echo={`$ temps node restart-agent ${node.name}`} title={`Restart the agent on ${node.name}`}
            description="The containers keep running; the agent reconnects in about five seconds and the heartbeat resumes. Reversible: nothing is redeployed."
            confirmWord={node.name} steps={['stop temps-agent', 'start temps-agent', 'wait for heartbeat']}
            onDone={() => notify('ok', 'agent restarted', `${node.name} · heartbeat in 4s`)} />
          <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => go('settings:cluster')}><Plus /> add a node</Button>
        </div>
      </Section>
    </>
  )

  return (
    <div className="space-y-4">
      {off && (
        <Callout state="error" title={`The agent stopped answering at ${rd.stale}`} quote="control plane unreachable: wss://app.acme.sh/agent timed out after 30s (relay)">
          Everything below is the last sample that arrived, greyed and stamped. The containers may still be running on the machine; the proxy cannot reach them. A relay node needs outbound 443 to the control plane, a direct node needs UDP 51820 both ways.
        </Callout>
      )}
      <Columns>
        <div className="min-w-0">
          {/* Below xl the aside is a stack under the main column; below md it is one
              panel behind "details", opening downward from the top of the charts so
              the reader never has to scroll to the end of the page to find it. */}
          <div className="relative mb-4 md:hidden">
            <Button ref={asideBtn} size="sm" variant="outline" className="h-7 text-xs" aria-expanded={details} onClick={() => setDetails((d) => !d)}>details</Button>
            <Drop anchor={asideBtn} open={details} label={`${node.name} details`} className="max-h-[70vh] overflow-auto p-3">{aside}</Drop>
          </div>
          <Section title="Resources"
            meta={off ? `last known · ${rd.stale} · 4m old` : `sampled every 15s · ${fmtCount(BUCKETS, 'bucket')} of ${BUCKET_MIN}m`}
            action={
              <span className="flex flex-wrap items-center gap-2">
                {/* Nothing is arriving from a machine that stopped answering, so the control does not pretend to poll it. */}
                {off ? <span className="font-mono text-[11px] text-muted-foreground"><span aria-hidden>○</span> not live · no samples since {rd.stale}</span> : <Live every="30s" paused={paused} onToggle={toggle} />}
                <Ranges value={range} onChange={setRange} onGated={(v) => notify('warn', `${v} is past the sample horizon`, `node samples are kept ${RETENTION.label}`)} />
              </span>
            }>
            <div className={off ? 'text-muted-foreground opacity-80' : undefined}>
              <ChartStack panes={panes} length={BUCKETS} />
            </div>
          </Section>
          <Section title="Containers" meta={off ? `${fmtCount(cs.length, 'container')} · unreachable` : fmtCount(cs.length, 'container')}>
            <ContainerLedger rows={cs} dense={dense} go={go} meta={null} />
          </Section>
        </div>
        <div className="hidden min-w-0 md:block">{aside}</div>
      </Columns>
    </div>
  )
}

// ── The service's resources ────────────────────────────────────────────

/** A stable seed from a container name, so a replica's network shape never moves between reloads. */
const seedOf = (s: string) => [...s].reduce((a, c) => (a * 31 + c.charCodeAt(0)) >>> 0, 7)
const netSparkOf = (c: ContainerRes) =>
  spark(seedOf(c.name), c.net / 1000, (c.net / 1000) * 0.4, (i) => (i === at('20:30') ? (c.net / 1000) * 4 : 0))

/**
 * The same four charts, for one service instead of one machine: its replicas
 * summed, the replicas listed under them, and the node each one runs on as a
 * link — because "the service is fine but one replica is on the machine that
 * is not" is the thing this view exists to show.
 *
 * A service has no disk of its own unless it carries a volume, so the fourth
 * chart says which of the four reasons it is empty and where the bytes
 * actually land, instead of drawing a flat line at zero.
 */
export function ServiceResources({ project, go, dense }: { project: string; go: (v: string) => void; dense: boolean }) {
  const reps = replicasOf(project)
  const [range, setRange] = useState<RangeKey>('24h')
  const { paused, toggle } = useLive()
  if (!reps.length) {
    return (
      <PageState state="empty" title="No containers running"
        reason={`${project} has no running containers on any node, so there is nothing to sample. Deploy it and the four charts fill from the first heartbeat, 30 seconds later.`} />
    )
  }
  const cpu = AXIS.map((_, i) => Number(reps.reduce((a, c) => a + (c.cpuSpark[i] ?? 0), 0).toFixed(1)))
  const mem = AXIS.map((_, i) => Math.round(reps.reduce((a, c) => a + (c.memSpark[i] ?? 0), 0)))
  const nets = reps.map(netSparkOf)
  const net = AXIS.map((_, i) => Math.round(nets.reduce((a, xs) => a + (xs[i] ?? 0), 0)))
  const limited = reps.every((c) => c.limit !== null)
  const limitMiB = limited ? reps.reduce((a, c) => a + (c.limit ?? 0), 0) / MIB : null
  const volume = reps.some((c) => c.kind === 'postgres' || c.kind === 'redis' || c.kind === 'clickhouse')
  const nodes = [...new Set(reps.map((c) => c.node))]
  const memState: State = limitMiB && last(mem) / limitMiB >= 0.9 ? 'warn' : 'ok'

  const panes: Pane[] = [
    {
      key: 'cpu', header: <>cpu · % of a vCPU, {fmtCount(reps.length, 'replica')} summed</>,
      read: (i) => `cpu ${fmtPct(cpu[i], { digits: 1 })}`,
      render: (readout) => (
        <TimeChart data={points(cpu, 'cpu')} series={[{ key: 'cpu', name: 'cpu' }]} unit="%" height={132} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          title={`cpu for ${project}`} range={`last ${range}`} verdict={`Between ${fmtPct(Math.min(...cpu), { digits: 1 })} and ${fmtPct(Math.max(...cpu), { digits: 1 })} of a vCPU across ${fmtCount(reps.length, 'replica')}.`} />
      ),
      footer: <><span>cpu / 30 min · last {range}</span><span>· retention {RETENTION.label}</span><span>· ┆ deploy</span></>,
    },
    {
      key: 'mem', header: <>memory · MiB{limitMiB ? ` of ${fmtBytes(limitMiB * MIB, { binary: true })} of limits` : ' · no limit set'}</>,
      read: (i) => `memory ${fmtBytes(mem[i] * MIB, { binary: true })}`,
      render: (readout) => (
        <TimeChart data={points(mem, 'mem')} series={[{ key: 'mem', name: 'memory', state: memState === 'ok' ? undefined : memState }]} unit="MiB" height={132} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          thresholds={limitMiB ? [{ y: Math.round(limitMiB), label: 'sum of limits', state: 'error' }] : []}
          title={`memory for ${project}`} range={`last ${range}`}
          verdict={`${fmtBytes(last(mem) * MIB, { binary: true })} across ${fmtCount(reps.length, 'replica')}${limitMiB ? `, ${fmtPct((last(mem) / limitMiB) * 100, { digits: 0 })} of the limits they were given` : ''}.`} />
      ),
      footer: <><span>memory / 30 min · last {range}</span><span>· retention {RETENTION.label}</span>{limitMiB && <span>· the line is the sum of the replicas' own limits, not the nodes'</span>}</>,
    },
    {
      key: 'disk', header: <>disk · {volume ? 'GB on its volume' : 'no volume of its own'}</>,
      read: () => (volume ? 'disk on volume' : 'disk – · no volume'),
      render: () => volume
        ? <TimeChart data={points(AXIS.map((_, i) => Number((12 + i * 0.004).toFixed(2))), 'disk')} series={[{ key: 'disk', name: 'volume' }]} unit="GB" height={132} xInterval={8} markers={DEPLOYS}
            title={`volume for ${project}`} range={`last ${range}`} verdict="Growing steadily; the volume is the service's own, not the node's." />
        : <PageState state="empty" title="No volume of its own"
            reason={`${project} writes only to its container layer, so its disk is the node's disk. Open ${nodes.join(' or ')} to see where the bytes land, or attach a volume in the service settings.`} />,
      footer: volume ? <><span>volume / 30 min · last {range}</span><span>· retention {RETENTION.label}</span></> : <span>a stateless service has no disk series · this is not "no data", it is "no volume"</span>,
    },
    {
      key: 'net', header: <>network · kB/s, {fmtCount(reps.length, 'replica')} summed</>,
      read: (i) => `network ${bytesPerSec(net[i] * 1000)}`,
      render: (readout) => (
        <TimeChart data={points(net, 'net')} series={[{ key: 'net', name: 'in + out' }]} unit="kB/s" height={132} xInterval={8} markers={DEPLOYS} readoutFormat={readout}
          title={`network for ${project}`} range={`last ${range}`} verdict={`Flat until ${DEPLOYS[1].id} at ${DEPLOYS[1].at}, then a burst while the replicas pulled the image.`} />
      ),
      footer: <><span>bytes / 30 min · last {range}</span><span>· retention {RETENTION.label}</span><span>· ┆ deploy</span></>,
    },
  ]

  return (
    <Section title="Resources"
      meta={`${fmtCount(reps.length, 'replica')} on ${nodes.length === 1 ? nodes[0] : fmtCount(nodes.length, 'node')} · sampled every 15s`}
      action={<span className="flex flex-wrap items-center gap-2"><Live every="30s" paused={paused} onToggle={toggle} /><Ranges value={range} onChange={setRange} onGated={() => undefined} /></span>}>
      <div className="space-y-4">
        <ChartStack panes={panes} length={BUCKETS} />
        <ContainerLedger rows={reps} dense={dense} go={go} showNode meta={<>{fmtCount(reps.length, 'replica')} · <button type="button" className="underline underline-offset-4" onClick={() => go(`node:${nodes[0]}`)}>{nodes[0]}</button>{nodes.length > 1 && ' and others'}</>} />
      </div>
    </Section>
  )
}

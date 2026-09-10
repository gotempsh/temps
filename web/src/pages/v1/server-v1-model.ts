// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The numbers behind the control-plane "Server" page, kept free of React so
 * the axis, the rates, the projection and the verdict can be unit tested
 * without rendering a chart. `ServerV1.tsx` only lays these out.
 */

import type { MetricDataPoint } from '@/api/client/types.gen'
import type { Series, TimePoint } from '@temps-sdk/ds'
import { fmtBytes, fmtCount, fmtNum, fmtPct } from '@temps-sdk/ds'

/** The control plane is always node 0. */
export const CONTROL_PLANE_NODE_ID = 0

export const RANGES = ['1h', '6h', '24h', '7d'] as const
export type RangeKey = (typeof RANGES)[number]

/** Bucket width the store uses for each window (`duration_to_step`). */
export const STEP_SECONDS: Record<RangeKey, number> = {
  '1h': 60,
  '6h': 300,
  '24h': 900,
  '7d': 3600,
}
export const RANGE_DAYS: Record<RangeKey, number> = {
  '1h': 1 / 24,
  '6h': 0.25,
  '24h': 1,
  '7d': 7,
}

export type Threshold = { at: number; state: 'warn' | 'error'; label: string }
/** Each line carries a word: the reader learns what the number means, not just that it is red. */
export const CPU_LINES: Threshold[] = [
  { at: 80, state: 'warn', label: 'busy' },
  { at: 95, state: 'error', label: 'saturated' },
]
export const MEM_LINES: Threshold[] = [
  { at: 85, state: 'warn', label: 'tight' },
  { at: 95, state: 'error', label: 'oom risk' },
]
export const DISK_LINES: Threshold[] = [
  { at: 80, state: 'warn', label: 'tight' },
  { at: 90, state: 'error', label: 'writes stop' },
]
export const FD_LINES: Threshold[] = [
  { at: 80, state: 'warn', label: 'tight' },
  { at: 95, state: 'error', label: 'no new sockets' },
]

export function crossed(
  value: number | null | undefined,
  lines: Threshold[]
): 'ok' | 'warn' | 'error' {
  if (value == null || !Number.isFinite(value)) return 'ok'
  const hit = [...lines].sort((a, b) => b.at - a.at).find((t) => value >= t.at)
  return hit?.state ?? 'ok'
}

// ── One axis for every chart ───────────────────────────────────────────

/** Tick text for a bucket: the clock inside a day, the date once the window spans days. */
export function axisLabel(ms: number, range: RangeKey): string {
  const d = new Date(ms)
  const hhmm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
  if (range === '1h' || range === '6h') return hhmm
  return `${d.toLocaleString('en', { month: 'short' })} ${d.getDate()} ${hhmm}`
}

export type Axis = { ms: number[]; labels: string[] }

/**
 * The union of every series' buckets, sorted. Four charts on one axis is the
 * point of the page ("what was cpu doing when memory climbed" is one
 * question), so a bucket one metric missed still exists on the others.
 */
export function buildAxis(
  seriesList: (MetricDataPoint[] | undefined)[],
  range: RangeKey
): Axis {
  const set = new Set<number>()
  for (const points of seriesList) {
    for (const p of points ?? []) {
      const ms = Date.parse(p.time)
      if (!Number.isNaN(ms)) set.add(ms)
    }
  }
  const ms = [...set].sort((a, b) => a - b)
  return { ms, labels: ms.map((m) => axisLabel(m, range)) }
}

/** Values by bucket timestamp, so a pane can read "the value at index i". */
export function valuesByMs(
  points: MetricDataPoint[] | undefined
): Map<number, number> {
  const out = new Map<number, number>()
  for (const p of points ?? []) {
    const ms = Date.parse(p.time)
    if (!Number.isNaN(ms) && Number.isFinite(p.value)) out.set(ms, p.value)
  }
  return out
}

/** Chart rows on the shared axis. A bucket the series missed gets no key, so the line breaks instead of dropping to zero. */
export function rows(
  axis: Axis,
  columns: {
    key: string
    values: Map<number, number>
    scale?: (v: number) => number
  }[]
): TimePoint[] {
  return axis.ms.map((ms, i) => {
    const row: TimePoint = { t: axis.labels[i] }
    for (const c of columns) {
      const v = c.values.get(ms)
      if (v != null) row[c.key] = c.scale ? c.scale(v) : v
    }
    return row
  })
}

/**
 * Per-bucket increases of a `*_total` counter (what the range endpoint
 * returns for cumulative metrics) as bytes per second. The bucket width is
 * the gap to the previous bucket; a lone point uses the range's step.
 */
export function toRatePerSecond(
  points: MetricDataPoint[] | undefined,
  fallbackStepSeconds: number
): MetricDataPoint[] {
  if (!points || points.length === 0) return []
  const times = points.map((p) => Date.parse(p.time))
  const stepFor = (i: number): number => {
    if (points.length === 1) return fallbackStepSeconds
    const gap = i > 0 ? times[i] - times[i - 1] : times[i + 1] - times[i]
    const secs = gap / 1000
    return Number.isFinite(secs) && secs > 0 ? secs : fallbackStepSeconds
  }
  return points.map((p, i) => ({
    time: p.time,
    value: Math.max(0, p.value) / stepFor(i),
  }))
}

/**
 * Tone belongs to the stretch that crossed the line, not to the whole
 * window: a second series carries only the buckets at or above the
 * threshold, drawn on top in the threshold's tone and left out of the table
 * because it is the same numbers as the line it marks.
 */
export function overed(
  data: TimePoint[],
  key: string,
  lines: Threshold[],
  unit: string,
  stepSeconds: number
): {
  data: TimePoint[]
  extra: Series[]
  buckets: number
  note: string | null
} {
  const [warn, error] = [...lines].sort((a, b) => a.at - b.at)
  let buckets = 0
  let hitError = false
  const out = data.map((p) => {
    const v = p[key]
    if (typeof v === 'number' && v >= warn.at) {
      buckets++
      if (error && v >= error.at) hitError = true
      return { ...p, over: v }
    }
    return p
  })
  const line = hitError && error ? error : warn
  const extra: Series[] = buckets
    ? [
        {
          key: 'over',
          name: `above ${line.label} ${fmtNum(line.at)}${unit}`,
          state: line.state,
          stroke: 'solid',
          weight: 'regular',
          top: true,
          inTable: false,
        },
      ]
    : []
  const note = buckets
    ? `· ◐ above ${warn.label} for ${fmtCount(buckets, 'bucket')} (${fmtSpan(buckets * stepSeconds)})`
    : null
  return { data: out, extra, buckets, note }
}

// ── Disk projection ────────────────────────────────────────────────────

export type Projection = {
  /** Bytes per day from a least-squares line through the window. Zero or negative means "not growing". */
  bytesPerDay: number
  /** Days until the usage reaches the error line (90%), from the last sample. */
  daysToLine: number
  /** Days until the disk is full. */
  daysToFull: number
  /** Trend value at every bucket of the axis, as a percentage of the disk. */
  trendPct: (ms: number) => number
}

export function projectDisk(
  used: Map<number, number>,
  total: number | null | undefined
): Projection | null {
  if (!total || total <= 0 || used.size < 3) return null
  const xs = [...used.keys()].sort((a, b) => a - b)
  const ys = xs.map((x) => used.get(x) as number)
  const n = xs.length
  const x0 = xs[0]
  const mx = xs.reduce((a, x) => a + (x - x0), 0) / n
  const my = ys.reduce((a, y) => a + y, 0) / n
  let sxx = 0
  let sxy = 0
  for (let i = 0; i < n; i++) {
    const dx = xs[i] - x0 - mx
    sxx += dx * dx
    sxy += dx * (ys[i] - my)
  }
  if (sxx === 0) return null
  const slopePerMs = sxy / sxx
  const bytesPerDay = slopePerMs * 86_400_000
  const lastY = ys[n - 1]
  const line = total * (DISK_LINES[1].at / 100)
  const daysTo = (target: number) =>
    bytesPerDay > 0 && target > lastY
      ? (target - lastY) / bytesPerDay
      : bytesPerDay > 0
        ? 0
        : Infinity
  return {
    bytesPerDay,
    daysToLine: daysTo(line),
    daysToFull: daysTo(total),
    trendPct: (ms) =>
      Number((((my + slopePerMs * (ms - x0 - mx)) / total) * 100).toFixed(2)),
  }
}

/** "full in 44 d", "full in 3 months": a projection is stated in the unit an operator plans in. */
export function inDays(d: number): string {
  if (!Number.isFinite(d)) return 'not growing'
  if (d < 1) return 'today'
  if (d < 90) return `${fmtNum(Math.round(d))} d`
  if (d < 730) return `${fmtNum(Math.round(d / 30))} months`
  return `${fmtNum(Math.round(d / 365))} years`
}

/** The date a projection lands on. */
export function dateIn(days: number, now: number = Date.now()): string {
  if (!Number.isFinite(days)) return '–'
  const d = new Date(now + days * 86_400_000)
  const thisYear = new Date(now).getFullYear()
  return `${d.toLocaleString('en', { month: 'short' })} ${d.getDate()}${d.getFullYear() === thisYear ? '' : ` ${d.getFullYear()}`}`
}

/** A span of seconds in the words a footer uses ("12 min", "3 h", "2 d"). */
export function fmtSpan(seconds: number): string {
  if (seconds < 90) return `${Math.round(seconds)} s`
  if (seconds < 5400) return `${Math.round(seconds / 60)} min`
  if (seconds < 172_800) return `${fmtNum(seconds / 3600, { digits: 1 })} h`
  return `${fmtNum(seconds / 86_400, { digits: 1 })} d`
}

export const bytesPerSec = (n: number | null | undefined) =>
  n == null || !Number.isFinite(n) ? '—' : `${fmtBytes(n)}/s`

// ── The verdict ────────────────────────────────────────────────────────

export type Snapshot = {
  cpu: number | null
  memPct: number | null
  memUsed: number | null
  memTotal: number | null
  diskPct: number | null
  diskUsed: number | null
  diskTotal: number | null
  fdPct: number | null
  netInRate: number | null
  netOutRate: number | null
}

export function snapshotOf(
  latest: Record<string, number> | undefined
): Snapshot {
  const g = (k: string) => {
    const v = latest?.[k]
    return v != null && Number.isFinite(v) ? v : null
  }
  return {
    cpu: g('node.cpu_percent'),
    memPct: g('node.memory_percent'),
    memUsed: g('node.memory_used_bytes'),
    memTotal: g('node.memory_total_bytes'),
    diskPct: g('node.disk_percent'),
    diskUsed: g('node.disk_used_bytes'),
    diskTotal: g('node.disk_total_bytes'),
    fdPct: g('node.fd_percent'),
    netInRate: null,
    netOutRate: null,
  }
}

export type Verdict = {
  state: 'ok' | 'warn' | 'error' | 'idle'
  /** Two or three words for the lede ("disk tight"). */
  word: string
  /** One short sentence for the header's attention row. */
  short: string
  /** The full sentence: what is wrong and what to do about it. */
  text: string
}

/**
 * The first line of the page: what to do, before any number. A silent
 * sampler is a verdict too, and "nothing to do" is one as well.
 */
export function verdictOf(
  snap: Snapshot,
  opts: {
    staleSeconds: number | null
    scrapeIntervalSecs: number
    projection: Projection | null
    dockerReclaimable: number | null
    cpuBusyBuckets: number
    stepSeconds: number
  }
): Verdict {
  const { staleSeconds, scrapeIntervalSecs, projection, dockerReclaimable } =
    opts
  if (staleSeconds == null) {
    return {
      state: 'idle',
      word: 'not sampled yet',
      short: 'No node sample has landed yet.',
      text: `No node sample has landed. The first one arrives within one scrape interval (${fmtSpan(scrapeIntervalSecs)}) of the proxy process starting.`,
    }
  }
  if (staleSeconds > Math.max(3 * scrapeIntervalSecs, 180)) {
    return {
      state: 'idle',
      word: 'sampler silent',
      short: `No sample for ${fmtSpan(staleSeconds)}.`,
      text: `No sample for ${fmtSpan(staleSeconds)} while one is expected every ${fmtSpan(scrapeIntervalSecs)}. The node sampler runs inside the proxy process; check that temps serve is up and writing to the metrics store. Everything below is the last value that was true.`,
    }
  }
  const disk = crossed(snap.diskPct, DISK_LINES)
  const mem = crossed(snap.memPct, MEM_LINES)
  const cpu = crossed(snap.cpu, CPU_LINES)
  const fd = crossed(snap.fdPct, FD_LINES)
  const worst = [disk, mem, cpu, fd].includes('error')
    ? 'error'
    : [disk, mem, cpu, fd].includes('warn')
      ? 'warn'
      : 'ok'
  const diskWords = `${fmtPct(snap.diskPct, { digits: 0 })} of ${fmtBytes(snap.diskTotal)}`
  if (disk !== 'ok') {
    const free = fmtBytes(
      Math.max(0, (snap.diskTotal ?? 0) - (snap.diskUsed ?? 0))
    )
    const fix =
      dockerReclaimable && dockerReclaimable > 0
        ? `Docker can free ${fmtBytes(dockerReclaimable)}: prune it first.`
        : 'Prune Docker or add space.'
    const when =
      projection && projection.bytesPerDay > 0
        ? ` At ${fmtBytes(projection.bytesPerDay)}/day it is full in ${inDays(projection.daysToFull)}.`
        : ''
    return {
      state: disk,
      word: disk === 'error' ? 'disk critical' : 'disk tight',
      short: `Disk is at ${diskWords}.`,
      text: `Disk is at ${diskWords} (${free} free).${when} ${fix}`,
    }
  }
  if (mem !== 'ok') {
    const free = fmtBytes(
      Math.max(0, (snap.memTotal ?? 0) - (snap.memUsed ?? 0)),
      { binary: true }
    )
    return {
      state: mem,
      word: mem === 'error' ? 'memory critical' : 'memory tight',
      short: `Memory is at ${fmtPct(snap.memPct, { digits: 0 })} of ${fmtBytes(snap.memTotal, { binary: true })}.`,
      text: `Memory is at ${fmtPct(snap.memPct, { digits: 0 })} of ${fmtBytes(snap.memTotal, { binary: true })} with ${free} left: move or cap a container before the kernel picks one to kill.`,
    }
  }
  if (cpu !== 'ok') {
    const since = opts.cpuBusyBuckets
      ? ` for ${fmtSpan(opts.cpuBusyBuckets * opts.stepSeconds)}`
      : ''
    return {
      state: cpu,
      word: cpu === 'error' ? 'cpu saturated' : 'cpu busy',
      short: `CPU is at ${fmtPct(snap.cpu, { digits: 0 })}.`,
      text: `CPU is at ${fmtPct(snap.cpu, { digits: 0 })}${since}: builds and requests queue behind it. Find the container burning it, or add cores.`,
    }
  }
  if (fd !== 'ok') {
    return {
      state: fd,
      word: 'file handles tight',
      short: `File handles are at ${fmtPct(snap.fdPct, { digits: 0 })} of the ceiling.`,
      text: `The host has used ${fmtPct(snap.fdPct, { digits: 0 })} of its file handles; sockets are file handles, so the proxy stops accepting connections at the ceiling. Raise fs.file-max or find the leak.`,
    }
  }
  const projected =
    projection &&
    projection.bytesPerDay > 0 &&
    Number.isFinite(projection.daysToLine) &&
    projection.daysToLine < 30
      ? ` Disk grows ${fmtBytes(projection.bytesPerDay)}/day and reaches the ${DISK_LINES[1].at}% line in ${inDays(projection.daysToLine)}.`
      : ''
  return {
    state: projected ? 'warn' : worst,
    word: projected ? 'disk filling' : 'nothing to do',
    short: projected
      ? `Disk reaches the ${DISK_LINES[1].at}% line in ${inDays(projection?.daysToLine ?? Infinity)}.`
      : 'Nothing to do.',
    text: `cpu ${fmtPct(snap.cpu, { digits: 0 })} · memory ${fmtPct(snap.memPct, { digits: 0 })} of ${fmtBytes(snap.memTotal, { binary: true })} · disk ${diskWords}.${projected}`,
  }
}

/** How old the newest bucket is, in seconds; `null` when there is none. */
export function staleness(axis: Axis, now: number = Date.now()): number | null {
  if (axis.ms.length === 0) return null
  return Math.max(0, (now - axis.ms[axis.ms.length - 1]) / 1000)
}

/** Whether a metrics-store error is the endpoint's 503 "not enabled" answer rather than a real failure. */
export function isMetricsUnavailable(err: unknown): boolean {
  const problem = err as
    { status?: number; detail?: string; title?: string } | undefined
  if (problem?.status === 503) return true
  const msg = `${problem?.detail ?? ''} ${problem?.title ?? ''}`.toLowerCase()
  return msg.includes('not enabled') || msg.includes('unavailable')
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The control-plane host, as a record page on `@temps-sdk/ds`.
 *
 * Route: /monitoring/server. One verdict first, then cpu / memory / disk /
 * network / block I/O on one time axis under one cursor, and an aside with
 * the pressure gauges, what fills the disk (docker system df) and the
 * machine's facts. Everything the charts read comes from
 * `GET /nodes/0/metrics` (the node sampler in the proxy process); the docker
 * breakdown from `GET /nodes/0/docker-disk-usage`.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { useQuery } from '@tanstack/react-query'
import { RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import {
  nodeDockerDiskUsageGetOptions,
  nodeMetricsGetLatestOptions,
  nodeMetricsGetRangeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { DockerDiskUsage } from '@/api/client/types.gen'
import { useSettings } from '@/hooks/useSettings'
import {
  Callout,
  ChartFooter,
  Columns,
  Detail,
  Gauge,
  KeyValue,
  Lede,
  Live,
  MetricGrid,
  PageState,
  RangePicker,
  ReadoutLive,
  Section,
  StatusLine,
  TimeChart,
  fmtBytes,
  fmtNum,
  fmtPct,
  fmtStamp,
  useUrlState,
  type KV,
  type Range,
  type Series,
  type TimePoint,
} from '@temps-sdk/ds'
import {
  CONTROL_PLANE_NODE_ID,
  CPU_LINES,
  DISK_LINES,
  FD_LINES,
  MEM_LINES,
  RANGES,
  RANGE_DAYS,
  STEP_SECONDS,
  buildAxis,
  bytesPerSec,
  crossed,
  dateIn,
  fmtSpan,
  inDays,
  isMetricsUnavailable,
  overed,
  projectDisk,
  rows,
  snapshotOf,
  staleness,
  toRatePerSecond,
  valuesByMs,
  verdictOf,
  type Axis,
  type RangeKey,
} from './server-v1-model'

const METRICS_SETTINGS_PATH = '/settings/metrics-monitoring' as const
const RANGE_OPTIONS: Range[] = RANGES.map((label) => ({
  label,
  days: RANGE_DAYS[label],
}))
const LIVE_EVERY_MS = 30_000

// ── One cursor over every chart ────────────────────────────────────────

/**
 * `TimeChart` owns its own hover state, so five charts cannot be made to read
 * the same bucket from the package alone. The group holds one bucket index,
 * every pane reports the pointer's position on the shared axis, every
 * readout is formatted from that index, and a dotted ink cursor is drawn
 * over all of them at the same fraction of the plot. `←` `→` walk the
 * buckets without a pointer, `esc` clears, and the bucket is announced.
 */
const PLOT_INSET = 42 // px of y-axis gutter TimeChart reserves on the left

function useSharedCursor(length: number) {
  const [i, setI] = useState<number | null>(null)
  const fromPointer = useCallback(
    (el: HTMLElement, clientX: number) => {
      const r = el.getBoundingClientRect()
      const plot = r.width - PLOT_INSET
      if (plot <= 0 || length === 0) return
      const f = (clientX - r.left - PLOT_INSET) / plot
      setI(Math.max(0, Math.min(length - 1, Math.round(f * (length - 1)))))
    },
    [length]
  )
  const move = useCallback(
    (d: number) =>
      setI((cur) => Math.max(0, Math.min(length - 1, (cur ?? length - 1) + d))),
    [length]
  )
  return { i, setI, fromPointer, move }
}

function AxisPane({
  cursor,
  length,
  onPointer,
  onLeave,
  children,
}: {
  cursor: number | null
  length: number
  onPointer: (el: HTMLElement, x: number) => void
  onLeave: () => void
  children: ReactNode
}) {
  const ref = useRef<HTMLDivElement>(null)
  const frac = cursor === null ? null : cursor / Math.max(1, length - 1)
  return (
    <div
      ref={ref}
      className="relative min-w-0"
      onPointerMove={(e) => ref.current && onPointer(ref.current, e.clientX)}
      onPointerLeave={onLeave}
    >
      {children}
      {frac !== null && (
        <span
          aria-hidden
          className="pointer-events-none absolute inset-y-6 w-px border-l border-dashed border-foreground/50"
          style={{
            insetInlineStart: `calc(${PLOT_INSET}px + (100% - ${PLOT_INSET}px) * ${frac})`,
          }}
        />
      )}
    </div>
  )
}

type Pane = {
  key: string
  header: ReactNode
  /** The words for bucket `i`, for the live region and the readout. */
  read: (i: number) => string
  render: (readout: (p: TimePoint) => string) => ReactNode
  footer?: ReactNode
}

function ChartStack({ panes, axis }: { panes: Pane[]; axis: Axis }) {
  const length = axis.labels.length
  const { i, setI, fromPointer, move } = useSharedCursor(length)
  const idx = i ?? length - 1
  const label = axis.labels[idx] ?? '—'
  return (
    <div className="min-w-0 space-y-4">
      <div
        role="group"
        tabIndex={0}
        aria-label={`shared cursor over ${length} buckets · left and right arrows move it, escape clears it`}
        onKeyDown={(e) => {
          if (e.key === 'ArrowRight') {
            e.preventDefault()
            move(1)
          } else if (e.key === 'ArrowLeft') {
            e.preventDefault()
            move(-1)
          } else if (e.key === 'Escape') setI(null)
        }}
        className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1 border px-3 py-2 font-mono text-[11px] outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
      >
        <span className="op-label">cursor</span>
        <span className="tabular-nums">{label}</span>
        <span className="text-muted-foreground">
          {i === null
            ? 'latest bucket · hover any chart, or ← → to move'
            : 'every chart reads this bucket · esc to clear'}
        </span>
      </div>
      <ReadoutLive
        text={
          i === null
            ? ''
            : `${label} · ${panes.map((p) => p.read(idx)).join(' · ')}`
        }
      />
      {panes.map((p) => (
        <div key={p.key} className="min-w-0 border bg-background p-3">
          <div className="mb-1 flex min-w-0 flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
            <span className="op-label">{p.header}</span>
          </div>
          <AxisPane
            cursor={i}
            length={length}
            onPointer={fromPointer}
            onLeave={() => setI(null)}
          >
            {p.render(() => `${label} · ${p.read(idx)}`)}
          </AxisPane>
          {p.footer && <ChartFooter>{p.footer}</ChartFooter>}
        </div>
      ))}
    </div>
  )
}

// ── Data ───────────────────────────────────────────────────────────────

function useNodeRange(
  metric: string,
  range: RangeKey,
  refetchInterval: number | false
) {
  return useQuery({
    ...nodeMetricsGetRangeOptions({
      path: { id: CONTROL_PLANE_NODE_ID },
      query: { metric, range },
    }),
    refetchInterval,
    retry: false,
  })
}

const at = (m: Map<number, number>, ms: number | undefined) =>
  ms == null ? null : (m.get(ms) ?? null)

/** kB/s until the window peaks past a megabyte a second, then MB/s. */
function throughputUnit(values: number[]): { unit: string; div: number } {
  const peak = values.length ? Math.max(...values) : 0
  return peak >= 1_000_000
    ? { unit: 'MB/s', div: 1_000_000 }
    : { unit: 'kB/s', div: 1000 }
}

// ── What fills the disk ────────────────────────────────────────────────

function DockerUsage({
  usage,
  isPending,
  isFetching,
  error,
  onRefresh,
}: {
  usage: DockerDiskUsage | undefined
  isPending: boolean
  isFetching: boolean
  error: unknown
  onRefresh: () => void
}) {
  if (isPending) return <PageState state="loading" rows={4} />
  if (error || !usage) {
    const problem = error as { detail?: string; status?: number } | null
    return (
      <PageState
        state="error"
        title="docker system df did not answer"
        message={
          problem?.detail ??
          'The Docker socket did not respond. Docker walks every image layer and volume for this figure, which can take a minute on a busy host.'
        }
        resource={`GET /nodes/${CONTROL_PLANE_NODE_ID}/docker-disk-usage`}
        onRetry={onRefresh}
        retrying={isFetching}
      />
    )
  }
  const cats = [
    { label: 'images', c: usage.images },
    { label: 'containers', c: usage.containers },
    { label: 'volumes', c: usage.volumes },
    { label: 'build cache', c: usage.build_cache },
  ]
  const max = Math.max(1, ...cats.map((x) => x.c.size_bytes))
  const reclaimable = cats.reduce((a, x) => a + (x.c.reclaimable_bytes ?? 0), 0)
  return (
    <div className="space-y-2">
      <ol className="op-rows border bg-background text-xs">
        {cats.map(({ label, c }) => (
          <li
            key={label}
            className="relative grid grid-cols-[1fr_auto_auto] items-baseline gap-x-3 px-3 py-2"
          >
            <span
              aria-hidden
              className="absolute inset-y-1 left-0 bg-foreground/[0.06]"
              style={{ width: `${(c.size_bytes / max) * 100}%` }}
            />
            <span className="relative min-w-0 truncate">
              {label}
              <span className="text-muted-foreground">
                {' '}
                · {fmtNum(c.active_count)}/{fmtNum(c.total_count)} in use
              </span>
            </span>
            <span className="relative font-mono tabular-nums">
              {fmtBytes(c.size_bytes)}
            </span>
            <span className="relative w-10 text-right font-mono tabular-nums text-muted-foreground">
              {usage.total_bytes > 0
                ? fmtPct((c.size_bytes / usage.total_bytes) * 100, {
                    digits: 0,
                  })
                : '—'}
            </span>
          </li>
        ))}
      </ol>
      <p className="flex flex-wrap items-baseline gap-x-3 font-mono text-[10px] text-muted-foreground">
        <span>{fmtBytes(usage.total_bytes)} total</span>
        <span>· {fmtBytes(reclaimable)} reclaimable</span>
        <span>
          · as of{' '}
          {fmtStamp(new Date(usage.collected_at), { precision: 'second' })}
        </span>
        <button
          type="button"
          onClick={onRefresh}
          disabled={isFetching}
          className="ms-auto inline-flex items-center gap-1 underline underline-offset-4 hover:text-foreground disabled:opacity-60"
        >
          <RefreshCw
            className={`size-3 ${isFetching ? 'animate-spin' : ''}`}
            aria-hidden
          />
          {isFetching ? 'walking layers…' : 'refresh'}
        </button>
      </p>
    </div>
  )
}

// ── The page ───────────────────────────────────────────────────────────

export function ServerV1() {
  const [range, setRange] = useUrlState<RangeKey>('range', '1h', {
    values: RANGES,
  })
  const [paused, setPaused] = useState(false)
  const every = paused ? false : LIVE_EVERY_MS
  // The Live control advertises `space`: pause and resume the polling from
  // the keyboard, unless the reader is typing or on a control of their own.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== ' ' || e.metaKey || e.ctrlKey || e.altKey) return
      const tag = (e.target as HTMLElement | null)?.tagName
      if (
        tag === 'INPUT' ||
        tag === 'TEXTAREA' ||
        tag === 'BUTTON' ||
        tag === 'A' ||
        tag === 'SELECT' ||
        (e.target as HTMLElement | null)?.isContentEditable
      )
        return
      e.preventDefault()
      setPaused((p) => !p)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  const step = STEP_SECONDS[range]

  const settings = useSettings()
  const monitoring = settings.data?.monitoring
  const scrapeInterval = monitoring?.scrape_interval_secs ?? 30
  const retentionDays = monitoring?.retention_raw_days ?? 7

  const latest = useQuery({
    ...nodeMetricsGetLatestOptions({ path: { id: CONTROL_PLANE_NODE_ID } }),
    refetchInterval: every,
    retry: false,
  })
  const cpu = useNodeRange('node.cpu_percent', range, every)
  const mem = useNodeRange('node.memory_percent', range, every)
  const disk = useNodeRange('node.disk_used_bytes', range, every)
  const rx = useNodeRange('node.network_rx_bytes_total', range, every)
  const tx = useNodeRange('node.network_tx_bytes_total', range, every)
  const rd = useNodeRange('node.disk_read_bytes_total', range, every)
  const wr = useNodeRange('node.disk_write_bytes_total', range, every)
  const docker = useQuery({
    ...nodeDockerDiskUsageGetOptions({
      path: { node_id: CONTROL_PLANE_NODE_ID },
    }),
    staleTime: 5 * 60_000,
    retry: false,
  })

  const snap = useMemo(
    () => snapshotOf(latest.data as Record<string, number> | undefined),
    [latest.data]
  )

  // Every series on one axis. Counters arrive as per-bucket increases; they
  // become bytes/s here so the header can say the unit once.
  const model = useMemo(() => {
    const rxRate = toRatePerSecond(rx.data, step)
    const txRate = toRatePerSecond(tx.data, step)
    const rdRate = toRatePerSecond(rd.data, step)
    const wrRate = toRatePerSecond(wr.data, step)
    const axis = buildAxis(
      [cpu.data, mem.data, disk.data, rxRate, txRate, rdRate, wrRate],
      range
    )
    const v = {
      cpu: valuesByMs(cpu.data),
      mem: valuesByMs(mem.data),
      disk: valuesByMs(disk.data),
      rx: valuesByMs(rxRate),
      tx: valuesByMs(txRate),
      rd: valuesByMs(rdRate),
      wr: valuesByMs(wrRate),
    }
    const total = snap.diskTotal
    const projection = projectDisk(v.disk, total)
    const one = (n: number) => Number(n.toFixed(1))
    const cpuRows = overed(
      rows(axis, [{ key: 'cpu', values: v.cpu, scale: one }]),
      'cpu',
      CPU_LINES,
      '%',
      step
    )
    const memRows = overed(
      rows(axis, [{ key: 'mem', values: v.mem, scale: one }]),
      'mem',
      MEM_LINES,
      '%',
      step
    )
    const diskPct = (b: number) =>
      total ? Math.min(100, Number(((b / total) * 100).toFixed(1))) : 0
    const diskBase = rows(axis, [
      { key: 'used', values: v.disk, scale: diskPct },
    ])
    const diskRows = overed(
      projection && projection.bytesPerDay > 0
        ? diskBase.map((p, i) => ({
            ...p,
            trend: projection.trendPct(axis.ms[i]),
          }))
        : diskBase,
      'used',
      DISK_LINES,
      '%',
      step
    )
    // One unit per pane, chosen from the window's peak, so the axis reads
    // "280" not "280k" and the header says the unit once.
    const netUnit = throughputUnit([...v.rx.values(), ...v.tx.values()])
    const ioUnit = throughputUnit([...v.rd.values(), ...v.wr.values()])
    const netRows = rows(axis, [
      { key: 'in', values: v.rx, scale: (n) => one(n / netUnit.div) },
      { key: 'out', values: v.tx, scale: (n) => one(n / netUnit.div) },
    ])
    const ioRows = rows(axis, [
      { key: 'read', values: v.rd, scale: (n) => one(n / ioUnit.div) },
      { key: 'write', values: v.wr, scale: (n) => one(n / ioUnit.div) },
    ])
    const peakOf = (m: Map<number, number>) => {
      let best: { ms: number; v: number } | null = null
      for (const [ms, val] of m)
        if (!best || val > best.v) best = { ms, v: val }
      return best
    }
    return {
      axis,
      v,
      projection,
      cpuRows,
      memRows,
      diskRows,
      netRows,
      ioRows,
      netUnit,
      ioUnit,
      cpuPeak: peakOf(v.cpu),
      memPeak: peakOf(v.mem),
    }
  }, [
    cpu.data,
    mem.data,
    disk.data,
    rx.data,
    tx.data,
    rd.data,
    wr.data,
    range,
    step,
    snap.diskTotal,
  ])

  const { axis, v, projection } = model
  const lastMs = axis.ms[axis.ms.length - 1]
  const stale = staleness(axis)
  const silent = stale != null && stale > Math.max(3 * scrapeInterval, 180)
  const dockerReclaimable = docker.data
    ? [
        docker.data.images,
        docker.data.containers,
        docker.data.volumes,
        docker.data.build_cache,
      ].reduce((a, c) => a + (c.reclaimable_bytes ?? 0), 0)
    : null
  const verdict = verdictOf(snap, {
    staleSeconds: stale,
    scrapeIntervalSecs: scrapeInterval,
    projection,
    dockerReclaimable,
    cpuBusyBuckets: model.cpuRows.buckets,
    stepSeconds: step,
  })

  const refreshAll = () => {
    void latest.refetch()
    for (const q of [cpu, mem, disk, rx, tx, rd, wr]) void q.refetch()
  }

  // ── States that replace the body ─────────────────────────────────────
  // The node sampler and the store are always wired up in `temps serve`; the
  // global monitoring flag only governs per-service scraping. So "not set up"
  // is what the endpoint itself says (503), not a settings bit.
  const firstError = [cpu, latest].find((q) => q.isError)?.error
  const unconfigured = firstError != null && isMetricsUnavailable(firstError)
  if (unconfigured) {
    return (
      <Detail
        title="Server"
        meta="control plane · node 0"
        status={
          <StatusLine state="idle">
            Node sampling is off: nothing is recorded about this machine.
          </StatusLine>
        }
      >
        <PageState
          state="unconfigured"
          title="Metric collection is off"
          missing="This server has no metrics store, so the proxy process is not sampling this host. Pick a store (TimescaleDB is built in) in Metrics monitoring settings and restart temps serve."
          example={
            <span>
              With it on, this page reads cpu, memory, disk, network and block
              I/O of the control-plane host every {fmtSpan(scrapeInterval)},
              keeps {fmtNum(retentionDays)} days, and says when the disk fills.
            </span>
          }
          settingsHref={METRICS_SETTINGS_PATH}
          settingsLabel="Metrics monitoring settings"
        />
      </Detail>
    )
  }
  if (cpu.isPending || latest.isPending) {
    return (
      <Detail
        title="Server"
        meta="control plane · node 0"
        status={<StatusLine state="idle">Reading the last samples…</StatusLine>}
      >
        <PageState state="loading" rows={8} />
      </Detail>
    )
  }
  if (firstError) {
    const problem = firstError as { detail?: string; title?: string }
    return (
      <Detail
        title="Server"
        meta="control plane · node 0"
        status={
          <StatusLine state="error">
            The metrics store did not answer.
          </StatusLine>
        }
      >
        <PageState
          state="error"
          title="Could not read node metrics"
          message={problem.detail ?? problem.title ?? 'The request failed.'}
          resource={`GET /nodes/${CONTROL_PLANE_NODE_ID}/metrics`}
          onRetry={refreshAll}
          retrying={cpu.isFetching}
        />
      </Detail>
    )
  }

  // ── The charts ───────────────────────────────────────────────────────
  const dim = (s: string) =>
    silent ? `${s} · last true ${axis.labels[axis.labels.length - 1]}` : s
  const memTotalWords = fmtBytes(snap.memTotal, { binary: true })
  const diskTotalWords = fmtBytes(snap.diskTotal)
  const retention = `${fmtNum(retentionDays)}d`
  const bucketWords = `${fmtSpan(step)} buckets · last ${range}`
  const panes: Pane[] = [
    {
      key: 'cpu',
      header: 'cpu · % of all cores',
      read: (i) => `cpu ${fmtPct(at(v.cpu, axis.ms[i]), { digits: 0 })}`,
      render: (readout) => (
        <TimeChart
          data={model.cpuRows.data}
          series={[{ key: 'cpu', name: 'cpu' }, ...model.cpuRows.extra]}
          unit="%"
          height={160}
          yTicks={[0, 50, 100]}
          readoutFormat={readout}
          thresholds={CPU_LINES.map((t) => ({
            y: t.at,
            label: t.label,
            state: t.state,
          }))}
          title="cpu on the control plane"
          range={`last ${range}`}
          verdict={
            silent
              ? `No samples for ${fmtSpan(stale ?? 0)}; these are the last values that were true.`
              : model.cpuPeak
                ? `Now ${fmtPct(snap.cpu, { digits: 0 })}, peaking at ${fmtPct(model.cpuPeak.v, { digits: 0 })} at ${axis.labels[axis.ms.indexOf(model.cpuPeak.ms)]}.`
                : 'No buckets in this window.'
          }
        />
      ),
      footer: (
        <>
          <span>{bucketWords}</span>
          <span>· retention {retention}</span>
          {model.cpuRows.note && (
            <span className="text-warning">{model.cpuRows.note}</span>
          )}
        </>
      ),
    },
    {
      key: 'mem',
      header: <>memory · % of {memTotalWords}</>,
      read: (i) => {
        const p = at(v.mem, axis.ms[i])
        return `memory ${fmtPct(p, { digits: 0 })}${p != null && snap.memTotal ? ` (${fmtBytes((p / 100) * snap.memTotal, { binary: true })})` : ''}`
      },
      render: (readout) => (
        <TimeChart
          data={model.memRows.data}
          series={[{ key: 'mem', name: 'memory' }, ...model.memRows.extra]}
          unit="%"
          height={160}
          yTicks={[0, 50, 100]}
          readoutFormat={readout}
          thresholds={MEM_LINES.map((t) => ({
            y: t.at,
            label: t.label,
            state: t.state,
          }))}
          title="memory on the control plane"
          range={`last ${range}`}
          verdict={
            silent
              ? `No samples for ${fmtSpan(stale ?? 0)}; these are the last values that were true.`
              : `${fmtPct(snap.memPct, { digits: 0 })} of ${memTotalWords} in use${model.memPeak ? `, peak ${fmtPct(model.memPeak.v, { digits: 0 })} at ${axis.labels[axis.ms.indexOf(model.memPeak.ms)]}` : ''}.`
          }
        />
      ),
      footer: (
        <>
          <span>{bucketWords}</span>
          <span>· retention {retention}</span>
          {model.memRows.note && (
            <span className="text-warning">{model.memRows.note}</span>
          )}
        </>
      ),
    },
    {
      key: 'disk',
      header: <>disk · % of {diskTotalWords} under the data dir</>,
      read: (i) => {
        const b = at(v.disk, axis.ms[i])
        return `disk ${b != null && snap.diskTotal ? fmtPct((b / snap.diskTotal) * 100, { digits: 0 }) : '—'} · ${fmtBytes(b)} of ${diskTotalWords}`
      },
      render: (readout) => {
        const series: Series[] = [{ key: 'used', name: 'used' }]
        if (projection && projection.bytesPerDay > 0)
          series.push({
            key: 'trend',
            name: `projection · ${fmtBytes(projection.bytesPerDay)}/day`,
            stroke: 'dashed',
            weight: 'thin',
          })
        series.push(...model.diskRows.extra)
        return (
          <TimeChart
            data={model.diskRows.data}
            series={series}
            unit="%"
            height={160}
            yTicks={[0, 50, 100]}
            readoutFormat={readout}
            thresholds={DISK_LINES.map((t) => ({
              y: t.at,
              label: `${t.label} ${t.at}%`,
              state: t.state,
            }))}
            title="disk on the control plane"
            range={`last ${range}`}
            verdict={
              projection && projection.bytesPerDay > 0
                ? `${fmtPct(snap.diskPct, { digits: 0 })} used, growing ${fmtBytes(projection.bytesPerDay)} a day: it reaches the ${DISK_LINES[1].at}% line in ${inDays(projection.daysToLine)}, on ${dateIn(projection.daysToLine)}.`
                : `${fmtPct(snap.diskPct, { digits: 0 })} used and not growing over this window.`
            }
          />
        )
      },
      footer: (
        <>
          <span>{bucketWords}</span>
          <span>· retention {retention}</span>
          <span>
            ·{' '}
            {fmtBytes(
              Math.max(0, (snap.diskTotal ?? 0) - (snap.diskUsed ?? 0))
            )}{' '}
            free
            {projection && projection.bytesPerDay > 0
              ? ` · at ${fmtBytes(projection.bytesPerDay)}/day it hits the ${DISK_LINES[1].at}% line on ${dateIn(projection.daysToLine)} and is full on ${dateIn(projection.daysToFull)}`
              : ' · not growing over this window'}
          </span>
          {model.diskRows.note && (
            <span className="text-warning">{model.diskRows.note}</span>
          )}
        </>
      ),
    },
    {
      key: 'net',
      header: `network · ${model.netUnit.unit} on physical interfaces`,
      read: (i) =>
        `network ${bytesPerSec(at(v.rx, axis.ms[i]))} in · ${bytesPerSec(at(v.tx, axis.ms[i]))} out`,
      render: (readout) => (
        <TimeChart
          data={model.netRows}
          series={[
            { key: 'in', name: 'in' },
            { key: 'out', name: 'out', stroke: 'dashed', weight: 'thin' },
          ]}
          unit={model.netUnit.unit}
          height={160}
          readoutFormat={readout}
          title="network on the control plane"
          range={`last ${range}`}
          verdict={`Now ${bytesPerSec(at(v.rx, lastMs))} in and ${bytesPerSec(at(v.tx, lastMs))} out; peak ${bytesPerSec(Math.max(0, ...v.rx.values()))} in, ${bytesPerSec(Math.max(0, ...v.tx.values()))} out.`}
        />
      ),
      footer: (
        <>
          <span>{bucketWords}</span>
          <span>· retention {retention}</span>
          <span>· loopback, bridges and tunnels excluded</span>
        </>
      ),
    },
    {
      key: 'io',
      header: `block i/o · ${model.ioUnit.unit} on physical disks`,
      read: (i) =>
        `block i/o ${bytesPerSec(at(v.rd, axis.ms[i]))} read · ${bytesPerSec(at(v.wr, axis.ms[i]))} write`,
      render: (readout) => (
        <TimeChart
          data={model.ioRows}
          series={[
            { key: 'read', name: 'read' },
            { key: 'write', name: 'write', stroke: 'dashed', weight: 'thin' },
          ]}
          unit={model.ioUnit.unit}
          height={160}
          readoutFormat={readout}
          title="block i/o on the control plane"
          range={`last ${range}`}
          verdict={`Now ${bytesPerSec(at(v.rd, lastMs))} read and ${bytesPerSec(at(v.wr, lastMs))} written; peak ${bytesPerSec(Math.max(0, ...v.rd.values()))} read, ${bytesPerSec(Math.max(0, ...v.wr.values()))} written.`}
        />
      ),
      footer: (
        <>
          <span>{bucketWords}</span>
          <span>· retention {retention}</span>
          <span>· partitions, loop and device-mapper excluded</span>
        </>
      ),
    },
  ]

  // ── The aside ────────────────────────────────────────────────────────
  const idle = silent ? `no sample for ${fmtSpan(stale ?? 0)}` : undefined
  const fdAllocated = latest.data?.['node.fd_allocated']
  const fdMax = latest.data?.['node.fd_max']
  const load = [
    'node.load_avg_1m',
    'node.load_avg_5m',
    'node.load_avg_15m',
  ].map((k) => latest.data?.[k])
  const machine: KV[] = (
    [
      {
        k: 'sampler',
        v: `every ${fmtSpan(scrapeInterval)} · in the proxy process`,
        mono: true,
      },
      {
        k: 'store',
        v: `${monitoring?.store === 'click_house' ? 'ClickHouse' : 'TimescaleDB'} · raw ${retention}`,
        mono: true,
      },
      load.every((n) => n != null)
        ? {
            k: 'load 1 / 5 / 15 min',
            v: load.map((n) => fmtNum(n as number, { digits: 2 })).join(' / '),
            mono: true,
          }
        : null,
      fdAllocated != null && fdMax != null
        ? {
            k: 'file handles',
            v: `${fmtNum(fdAllocated)} of ${fmtNum(fdMax)}`,
            mono: true,
          }
        : null,
      { k: 'memory', v: memTotalWords, mono: true },
      { k: 'disk', v: `${diskTotalWords} under the data dir`, mono: true },
    ] as (KV | null)[]
  ).filter((r): r is KV => r !== null)
  const aside = (
    <>
      <Section
        title="Pressure"
        meta={
          silent
            ? `last known · ${axis.labels[axis.labels.length - 1]}`
            : 'now · peak in the window'
        }
      >
        <div className={silent ? 'text-muted-foreground' : undefined}>
          <MetricGrid cols={2}>
            <Gauge
              label="cpu"
              value={Number((snap.cpu ?? 0).toFixed(1))}
              of={dim('of all cores')}
              peak={
                model.cpuPeak ? Number(model.cpuPeak.v.toFixed(1)) : undefined
              }
              peakLabel={
                model.cpuPeak
                  ? `at ${axis.labels[axis.ms.indexOf(model.cpuPeak.ms)]}`
                  : undefined
              }
              thresholds={CPU_LINES}
              idle={idle}
            />
            <Gauge
              label="memory"
              value={Number((snap.memPct ?? 0).toFixed(1))}
              of={dim(
                `${fmtBytes(snap.memUsed, { binary: true })} of ${memTotalWords}`
              )}
              peak={
                model.memPeak ? Number(model.memPeak.v.toFixed(1)) : undefined
              }
              peakLabel={
                model.memPeak
                  ? `at ${axis.labels[axis.ms.indexOf(model.memPeak.ms)]}`
                  : undefined
              }
              thresholds={MEM_LINES}
              idle={idle}
            />
            <Gauge
              label="disk"
              value={Number((snap.diskPct ?? 0).toFixed(1))}
              of={dim(`${fmtBytes(snap.diskUsed)} of ${diskTotalWords}`)}
              thresholds={DISK_LINES}
              idle={idle}
            />
            <Gauge
              label="file handles"
              value={Number((snap.fdPct ?? 0).toFixed(1))}
              of={dim(
                fdMax != null ? `of ${fmtNum(fdMax)}` : 'of the host ceiling'
              )}
              thresholds={FD_LINES}
              idle={
                snap.fdPct == null
                  ? 'not reported on this host · /proc/sys/fs/file-nr is Linux only'
                  : idle
              }
            />
          </MetricGrid>
        </div>
      </Section>
      <Section title="What fills the disk" meta="docker system df">
        <DockerUsage
          usage={docker.data}
          isPending={docker.isPending}
          isFetching={docker.isFetching}
          error={docker.error}
          onRefresh={() => void docker.refetch()}
        />
      </Section>
      <Section title="Machine">
        <KeyValue compact rows={machine} />
      </Section>
    </>
  )

  const facts: KV[] = [
    {
      k: 'last sample',
      v: stale == null ? 'none yet' : `${fmtSpan(stale)} ago`,
      mono: true,
      state: silent ? 'idle' : undefined,
    },
    {
      k: 'cpu',
      v: fmtPct(snap.cpu, { digits: 0 }),
      mono: true,
      state: crossed(snap.cpu, CPU_LINES),
    },
    {
      k: 'memory',
      v: `${fmtPct(snap.memPct, { digits: 0 })} · ${fmtBytes(snap.memUsed, { binary: true })}`,
      mono: true,
      state: crossed(snap.memPct, MEM_LINES),
    },
    {
      k: 'disk',
      v: `${fmtPct(snap.diskPct, { digits: 0 })} · ${fmtBytes(snap.diskUsed)}`,
      mono: true,
      state: crossed(snap.diskPct, DISK_LINES),
    },
    {
      k: 'disk trend',
      v:
        projection && projection.bytesPerDay > 0
          ? `full in ${inDays(projection.daysToFull)} · ${dateIn(projection.daysToFull)}`
          : 'not growing',
      mono: true,
    },
    {
      k: 'docker',
      v: docker.data
        ? `${fmtBytes(docker.data.total_bytes)} · ${fmtBytes(dockerReclaimable ?? 0)} reclaimable`
        : docker.isError
          ? 'unreachable'
          : 'measuring…',
      mono: true,
    },
  ]

  return (
    <Detail
      title="Server"
      meta={`control plane · node ${CONTROL_PLANE_NODE_ID} · sampled every ${fmtSpan(scrapeInterval)}`}
      status={<StatusLine state={verdict.state}>{verdict.short}</StatusLine>}
      lede={
        <Lede state={verdict.state} word={verdict.word} facts={facts}>
          {verdict.text}
        </Lede>
      }
      actions={
        <span className="flex flex-wrap items-center gap-2">
          {silent ? (
            <span className="font-mono text-[11px] text-muted-foreground">
              <span aria-hidden>○</span> not live · no sample for{' '}
              {fmtSpan(stale ?? 0)}
            </span>
          ) : (
            <Live
              every="30s"
              paused={paused}
              onToggle={() => setPaused((p) => !p)}
            />
          )}
          <RangePicker
            ranges={RANGE_OPTIONS}
            value={range}
            onChange={(label) => setRange(label as RangeKey)}
            retentionDays={retentionDays}
            retentionLabel={retention}
            onGated={(r) =>
              toast.warning(`${r.label} is past the sample horizon`, {
                description: `node samples are kept ${retention}`,
              })
            }
          />
        </span>
      }
    >
      {silent && (
        <Callout
          state="warn"
          title={`The sampler has been silent for ${fmtSpan(stale ?? 0)}`}
        >
          The node sampler runs inside the proxy process and writes one row
          every {fmtSpan(scrapeInterval)}. Nothing has arrived since{' '}
          {lastMs ? fmtStamp(new Date(lastMs), { precision: 'minute' }) : '—'}:
          check that temps serve is running and that the metrics store accepts
          writes. Everything below is the last sample that arrived.
        </Callout>
      )}
      <Columns>
        <div
          className={
            silent ? 'min-w-0 text-muted-foreground opacity-80' : 'min-w-0'
          }
        >
          {axis.ms.length === 0 ? (
            <PageState
              state="empty"
              title="No buckets in this window"
              reason={`Nothing was sampled in the last ${range}. The first sample lands within ${fmtSpan(scrapeInterval)} of the proxy process starting.`}
            />
          ) : (
            <ChartStack panes={panes} axis={axis} />
          )}
        </div>
        <div className="min-w-0">{aside}</div>
      </Columns>
    </Detail>
  )
}

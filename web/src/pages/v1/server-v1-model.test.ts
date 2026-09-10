// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CPU_LINES,
  buildAxis,
  crossed,
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
} from './server-v1-model'

const T0 = Date.UTC(2026, 8, 10, 12, 0, 0)
const t = (secs: number) => new Date(T0 + secs * 1000).toISOString()

describe('axis', () => {
  test('is the sorted union of every series bucket', () => {
    const axis = buildAxis(
      [
        [
          { time: t(60), value: 1 },
          { time: t(0), value: 1 },
        ],
        [{ time: t(120), value: 1 }],
        undefined,
      ],
      '1h'
    )
    expect(axis.ms).toEqual([T0, T0 + 60_000, T0 + 120_000])
    expect(axis.labels).toHaveLength(3)
    expect(axis.labels[0]).toMatch(/^\d\d:\d\d$/)
  })

  test('rows leave a missed bucket without the key instead of a zero', () => {
    const axis = buildAxis(
      [
        [
          { time: t(0), value: 1 },
          { time: t(60), value: 2 },
        ],
      ],
      '1h'
    )
    const values = valuesByMs([{ time: t(60), value: 7 }])
    const out = rows(axis, [{ key: 'v', values, scale: (n) => n * 2 }])
    expect(out[0]).toEqual({ t: axis.labels[0] })
    expect(out[1]).toEqual({ t: axis.labels[1], v: 14 })
  })
})

describe('toRatePerSecond', () => {
  test('divides per-bucket increases by the bucket width', () => {
    const rate = toRatePerSecond(
      [
        { time: t(0), value: 6000 },
        { time: t(60), value: 12000 },
        { time: t(120), value: -5 },
      ],
      30
    )
    expect(rate.map((p) => p.value)).toEqual([100, 200, 0])
  })

  test('a lone point uses the fallback step', () => {
    expect(toRatePerSecond([{ time: t(0), value: 300 }], 30)[0].value).toBe(10)
    expect(toRatePerSecond(undefined, 30)).toEqual([])
  })
})

describe('overed', () => {
  test('marks only the buckets above the warn line and names the worst line crossed', () => {
    const data = [
      { t: 'a', cpu: 10 },
      { t: 'b', cpu: 85 },
      { t: 'c', cpu: 97 },
    ]
    const out = overed(data, 'cpu', CPU_LINES, '%', 60)
    expect(out.buckets).toBe(2)
    expect(out.data[0]).toEqual({ t: 'a', cpu: 10 })
    expect(out.data[1]).toEqual({ t: 'b', cpu: 85, over: 85 })
    expect(out.extra[0].state).toBe('error')
    expect(out.extra[0].name).toBe('above saturated 95%')
    expect(out.note).toContain('2 buckets')
  })

  test('is silent under the line', () => {
    const out = overed([{ t: 'a', cpu: 10 }], 'cpu', CPU_LINES, '%', 60)
    expect(out.extra).toEqual([])
    expect(out.note).toBeNull()
  })
})

describe('projectDisk', () => {
  const GB = 1e9
  test('fits a line and reports days to the 90% line and to full', () => {
    // 1 GB/hour on a 100 GB disk, from 50 GB.
    const used = new Map<number, number>()
    for (let h = 0; h <= 3; h++) used.set(T0 + h * 3_600_000, 50 * GB + h * GB)
    const p = projectDisk(used, 100 * GB)
    expect(p).not.toBeNull()
    expect(p!.bytesPerDay).toBeCloseTo(24 * GB, -6)
    // last sample 53 GB; 90 GB line is 37 GB away = 1.54 days; full = 47 GB = 1.96 days
    expect(p!.daysToLine).toBeCloseTo(37 / 24, 2)
    expect(p!.daysToFull).toBeCloseTo(47 / 24, 2)
    expect(p!.trendPct(T0)).toBeCloseTo(50, 0)
  })

  test('a flat or shrinking disk is not growing', () => {
    const used = new Map<number, number>([
      [T0, 10 * GB],
      [T0 + 60_000, 9 * GB],
      [T0 + 120_000, 8 * GB],
    ])
    const p = projectDisk(used, 100 * GB)
    expect(p!.bytesPerDay).toBeLessThan(0)
    expect(p!.daysToFull).toBe(Infinity)
    expect(inDays(p!.daysToFull)).toBe('not growing')
  })

  test('needs three samples and a total', () => {
    expect(projectDisk(new Map([[T0, 1]]), 100)).toBeNull()
    expect(
      projectDisk(
        new Map([
          [T0, 1],
          [T0 + 1, 2],
          [T0 + 2, 3],
        ]),
        0
      )
    ).toBeNull()
  })
})

describe('verdict', () => {
  const healthy = snapshotOf({
    'node.cpu_percent': 3,
    'node.memory_percent': 24,
    'node.memory_used_bytes': 2 * 1024 ** 3,
    'node.memory_total_bytes': 8 * 1024 ** 3,
    'node.disk_percent': 22,
    'node.disk_used_bytes': 22e9,
    'node.disk_total_bytes': 100e9,
    'node.fd_percent': 1,
  })
  const base = {
    staleSeconds: 12,
    scrapeIntervalSecs: 30,
    projection: null,
    dockerReclaimable: null,
    cpuBusyBuckets: 0,
    stepSeconds: 60,
  }

  test('nothing to do reads the three numbers', () => {
    const v = verdictOf(healthy, base)
    expect(v.state).toBe('ok')
    expect(v.word).toBe('nothing to do')
    expect(v.text).toContain('cpu 3%')
    expect(v.text).toContain('disk 22%')
  })

  test('a silent sampler is a verdict before any number', () => {
    const v = verdictOf(healthy, { ...base, staleSeconds: 600 })
    expect(v.state).toBe('idle')
    expect(v.text).toContain('No sample for 10 min')
    expect(verdictOf(healthy, { ...base, staleSeconds: null }).word).toBe(
      'not sampled yet'
    )
  })

  test('a tight disk names what would free it', () => {
    const v = verdictOf(
      { ...healthy, diskPct: 86, diskUsed: 86e9 },
      { ...base, dockerReclaimable: 12e9 }
    )
    expect(v.state).toBe('warn')
    expect(v.text).toContain('Docker can free 12 GB')
    expect(v.text).toContain('14 GB free')
  })

  test('disk beats memory beats cpu', () => {
    const v = verdictOf({ ...healthy, memPct: 96, cpu: 99 }, base)
    expect(v.word).toBe('memory critical')
    expect(
      verdictOf({ ...healthy, cpu: 99 }, { ...base, cpuBusyBuckets: 5 }).text
    ).toContain('for 5 min')
  })

  test('a filling disk warns even when nothing is crossed', () => {
    const projection = {
      bytesPerDay: 5e9,
      daysToLine: 12,
      daysToFull: 15,
      trendPct: () => 0,
    }
    const v = verdictOf(healthy, { ...base, projection })
    expect(v.state).toBe('warn')
    expect(v.text).toContain('reaches the 90% line in 12 d')
  })
})

describe('small helpers', () => {
  test('crossed picks the highest line at or under the value', () => {
    expect(crossed(50, CPU_LINES)).toBe('ok')
    expect(crossed(80, CPU_LINES)).toBe('warn')
    expect(crossed(95, CPU_LINES)).toBe('error')
    expect(crossed(null, CPU_LINES)).toBe('ok')
  })

  test('staleness is the age of the newest bucket', () => {
    const axis = buildAxis([[{ time: t(0), value: 1 }]], '1h')
    expect(staleness(axis, T0 + 45_000)).toBe(45)
    expect(staleness({ ms: [], labels: [] })).toBeNull()
  })

  test('503 or "not enabled" means not configured, anything else is a failure', () => {
    expect(isMetricsUnavailable({ status: 503 })).toBe(true)
    expect(
      isMetricsUnavailable({
        status: 500,
        detail: 'Metric collection is not enabled on this server',
      })
    ).toBe(true)
    expect(isMetricsUnavailable({ status: 500, detail: 'boom' })).toBe(false)
    expect(isMetricsUnavailable(undefined)).toBe(false)
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CONTAINER_CLUSTER_CPU_METRIC,
  CONTAINER_CPU_LIMIT_METRIC,
  CONTAINER_CPU_METRIC,
  CONTAINER_CPU_METRIC_LABELS,
  CONTAINER_CPU_UTILIZATION_METRIC,
  alertableMetrics,
  containerCpuAxisWidth,
  containerCpuChartMetric,
  containerCpuHistoryMetric,
  containerCpuTileText,
  formatContainerCpuMetric,
  withCpuHistory,
} from './container-cpu'

function lookupOf(values: Record<string, number>) {
  return (metric: string) => values[metric]
}

describe('containerCpuTileText', () => {
  test('reads the stored Docker percent as cores against the limit', () => {
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 68.1,
        [CONTAINER_CPU_LIMIT_METRIC]: 6,
      }),
      false
    )
    expect(text).toBe('0.68 / 6 cores')
  })

  test('an uncapped container shows cores without a denominator', () => {
    expect(
      containerCpuTileText(lookupOf({ [CONTAINER_CPU_METRIC]: 85 }), false)
    ).toBe('0.85 cores')
  })

  test('more than one core reads above 1, never as a percentage over 100', () => {
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 240,
        [CONTAINER_CPU_LIMIT_METRIC]: 4,
      }),
      false
    )
    expect(text).toBe('2.40 / 4 cores')
  })

  test("a cluster's tile pairs its busiest member's usage with that member's limit", () => {
    // The latest cpu_percent is some member's (0.20 cores); the cluster
    // reading and limit are the busiest member's: 1.5 of 2 cores.
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 20,
        [CONTAINER_CLUSTER_CPU_METRIC]: 150,
        [CONTAINER_CPU_LIMIT_METRIC]: 2,
        [CONTAINER_CPU_UTILIZATION_METRIC]: 75,
      }),
      true
    )
    expect(text).toBe('1.50 / 2 cores')
  })

  test("an uncapped cluster's tile shows its busiest member's cores, not any member's", () => {
    // The busiest member is uncapped (limit 0) and uses 8 cores; the latest
    // cpu_percent is another member's 0.5 cores.
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 50,
        [CONTAINER_CLUSTER_CPU_METRIC]: 800,
        [CONTAINER_CPU_LIMIT_METRIC]: 0,
        [CONTAINER_CPU_UTILIZATION_METRIC]: 50,
      }),
      true
    )
    expect(text).toBe('8.00 cores')
  })

  test('a cluster without its reading yet (just upgraded) reads the recorded usage', () => {
    const text = containerCpuTileText(
      lookupOf({ [CONTAINER_CPU_METRIC]: 20, [CONTAINER_CPU_LIMIT_METRIC]: 2 }),
      true
    )
    expect(text).toBe('0.20 / 2 cores')
  })

  test('a single container reads its own recorded usage, never an older share', () => {
    // The limit could not be re-read on the latest ticks, so its share and
    // limit kept older values (0.30 of 2 cores) while the usage recorded
    // every tick moved on to 1.50 cores.
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 150,
        [CONTAINER_CPU_LIMIT_METRIC]: 2,
        [CONTAINER_CPU_UTILIZATION_METRIC]: 15,
      }),
      false
    )
    expect(text).toBe('1.50 / 2 cores')
  })

  test('an uncapped reading uses the cores recorded, not the host share', () => {
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 85,
        [CONTAINER_CPU_LIMIT_METRIC]: 0,
        [CONTAINER_CPU_UTILIZATION_METRIC]: 5.3,
      }),
      true
    )
    expect(text).toBe('0.85 cores')
  })

  test('no sample yet shows a dash', () => {
    expect(
      containerCpuTileText(lookupOf({ [CONTAINER_CPU_LIMIT_METRIC]: 6 }), false)
    ).toBe('—')
  })
})

describe('formatContainerCpuMetric', () => {
  test('formats cores used and the limit in cores', () => {
    expect(formatContainerCpuMetric(CONTAINER_CPU_METRIC, 68.1)).toBe(
      '0.68 cores'
    )
    expect(formatContainerCpuMetric(CONTAINER_CPU_METRIC, 2.4)).toBe(
      '0.024 cores'
    )
    expect(formatContainerCpuMetric(CONTAINER_CPU_LIMIT_METRIC, 6)).toBe(
      '6 cores'
    )
    expect(formatContainerCpuMetric(CONTAINER_CPU_LIMIT_METRIC, 1)).toBe(
      '1 core'
    )
    expect(formatContainerCpuMetric(CONTAINER_CPU_LIMIT_METRIC, 1.5)).toBe(
      '1.5 cores'
    )
  })

  test('leaves the utilization and other metrics to the caller', () => {
    expect(
      formatContainerCpuMetric(CONTAINER_CPU_UTILIZATION_METRIC, 11.35)
    ).toBeNull()
    expect(formatContainerCpuMetric('pg.connections', 3)).toBeNull()
  })
})

test('labels name the unit of each CPU metric', () => {
  expect(CONTAINER_CPU_METRIC_LABELS[CONTAINER_CPU_METRIC]).toBe('CPU (cores)')
  expect(CONTAINER_CPU_METRIC_LABELS[CONTAINER_CPU_UTILIZATION_METRIC]).toBe(
    'CPU (% of limit or host)'
  )
})

describe('alertableMetrics', () => {
  test('offers the share of the limit first and never cores × 100', () => {
    expect(
      alertableMetrics([
        CONTAINER_CPU_METRIC,
        CONTAINER_CPU_UTILIZATION_METRIC,
        'container.memory_used_bytes',
        'pg.connections',
      ])
    ).toEqual([
      CONTAINER_CPU_UTILIZATION_METRIC,
      'container.memory_used_bytes',
      'pg.connections',
    ])
  })
})

test('an uncapped limit (recorded as 0) reads without a denominator', () => {
  expect(
    containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 68.1,
        [CONTAINER_CPU_LIMIT_METRIC]: 0,
      }),
      false
    )
  ).toBe('0.68 cores')
  expect(formatContainerCpuMetric(CONTAINER_CPU_LIMIT_METRIC, 0)).toBe(
    'unlimited'
  )
})

describe('containerCpuAxisWidth', () => {
  test('leaves room for the widest cores tick at the 10px axis font', () => {
    const width = containerCpuAxisWidth(CONTAINER_CPU_METRIC)
    // "0.050 cores": 11 characters, at most ~5px each at 10px, plus the
    // 8px tick offset. The 52px `_percent` width clipped these.
    expect(width).toBeDefined()
    expect(width ?? 0).toBeGreaterThanOrEqual(
      formatContainerCpuMetric(CONTAINER_CPU_METRIC, 5)!.length * 5 + 8
    )
  })

  test('other metrics keep their own width', () => {
    expect(
      containerCpuAxisWidth(CONTAINER_CPU_UTILIZATION_METRIC)
    ).toBeUndefined()
    expect(containerCpuAxisWidth('pg.connections')).toBeUndefined()
  })
})

describe('containerCpuChartMetric', () => {
  test("a cluster's CPU tile charts its busiest member, the reading it shows", () => {
    expect(containerCpuChartMetric(CONTAINER_CPU_METRIC, true)).toBe(
      CONTAINER_CLUSTER_CPU_METRIC
    )
  })

  test('a single container and every other tile chart their own metric', () => {
    expect(containerCpuChartMetric(CONTAINER_CPU_METRIC, false)).toBe(
      CONTAINER_CPU_METRIC
    )
    expect(
      containerCpuChartMetric(CONTAINER_CPU_UTILIZATION_METRIC, true)
    ).toBe(CONTAINER_CPU_UTILIZATION_METRIC)
  })

  test('the busiest-member chart reads in cores, with room for its ticks', () => {
    expect(formatContainerCpuMetric(CONTAINER_CLUSTER_CPU_METRIC, 150)).toBe(
      '1.50 cores'
    )
    expect(containerCpuAxisWidth(CONTAINER_CLUSTER_CPU_METRIC)).toBe(
      containerCpuAxisWidth(CONTAINER_CPU_METRIC)
    )
    expect(CONTAINER_CPU_METRIC_LABELS[CONTAINER_CLUSTER_CPU_METRIC]).toBe(
      'CPU (cores, busiest member)'
    )
  })
})

describe('withCpuHistory', () => {
  const at = (minute: number, value: number | null) => ({
    time: `2026-01-01T00:${String(minute).padStart(2, '0')}:00Z`,
    value,
  })

  test("a cluster's CPU chart falls back to the members' history before it charts its busiest member", () => {
    expect(
      containerCpuHistoryMetric(
        CONTAINER_CPU_METRIC,
        containerCpuChartMetric(CONTAINER_CPU_METRIC, true)
      )
    ).toBe(CONTAINER_CPU_METRIC)
    expect(
      containerCpuHistoryMetric(
        CONTAINER_CPU_METRIC,
        containerCpuChartMetric(CONTAINER_CPU_METRIC, false)
      )
    ).toBeUndefined()
    expect(
      containerCpuHistoryMetric(
        CONTAINER_CPU_UTILIZATION_METRIC,
        containerCpuChartMetric(CONTAINER_CPU_UTILIZATION_METRIC, true)
      )
    ).toBeUndefined()
  })

  test('history fills the range before the first busiest-member reading', () => {
    const busiest = [at(0, null), at(1, null), at(2, 150), at(3, null)]
    const history = [at(0, 40), at(1, 45), at(2, 50), at(3, 55)]
    expect(withCpuHistory(busiest, history)).toEqual([
      at(0, 40),
      at(1, 45),
      at(2, 150),
      // A later gap stays a gap: no member had a complete reading then.
      at(3, null),
    ])
  })

  test('with no busiest-member reading yet, the whole range is history', () => {
    const history = [at(0, 40), at(1, null)]
    expect(withCpuHistory([at(0, null), at(1, null)], history)).toEqual(history)
  })

  test('with neither series holding a reading, the busiest-member range stays', () => {
    const busiest = [at(0, null)]
    expect(withCpuHistory(busiest, [])).toEqual(busiest)
  })
})

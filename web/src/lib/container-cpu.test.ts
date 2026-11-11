// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CONTAINER_CPU_LIMIT_METRIC,
  CONTAINER_CPU_METRIC,
  CONTAINER_CPU_METRIC_LABELS,
  CONTAINER_CPU_UTILIZATION_METRIC,
  alertableMetrics,
  containerCpuAxisWidth,
  containerCpuTileText,
  formatContainerCpuMetric,
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
      })
    )
    expect(text).toBe('0.68 / 6 cores')
  })

  test('an uncapped container shows cores without a denominator', () => {
    expect(containerCpuTileText(lookupOf({ [CONTAINER_CPU_METRIC]: 85 }))).toBe(
      '0.85 cores'
    )
  })

  test('more than one core reads above 1, never as a percentage over 100', () => {
    const text = containerCpuTileText(
      lookupOf({
        [CONTAINER_CPU_METRIC]: 240,
        [CONTAINER_CPU_LIMIT_METRIC]: 4,
      })
    )
    expect(text).toBe('2.40 / 4 cores')
  })

  test('no sample yet shows a dash', () => {
    expect(
      containerCpuTileText(lookupOf({ [CONTAINER_CPU_LIMIT_METRIC]: 6 }))
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
      })
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  bySeverityDesc,
  orderVolumeGroups,
  volumeSeriesFill,
} from './log-volume-series'

test('levels stack from routine at the bottom to errors on top', () => {
  expect(orderVolumeGroups(['WARN', 'DEBUG', 'ERROR', 'INFO'], true)).toEqual([
    'DEBUG',
    'INFO',
    'WARN',
    'ERROR',
  ])
  expect(orderVolumeGroups(['b', 'a'], false)).toEqual(['a', 'b'])
})

test('level bars use the same colours as the level badges', () => {
  expect(volumeSeriesFill('ERROR', 0, true)).toBe('var(--destructive)')
  expect(volumeSeriesFill('WARN', 1, true)).toBe('var(--warning)')
  // The conventional severity colours: info blue, debug grey.
  expect(volumeSeriesFill('INFO', 2, true)).toBe('var(--chart-1)')
  expect(volumeSeriesFill('DEBUG', 3, true)).toBe('var(--muted-foreground)')
  // Any other grouping cycles the shared breakdown palette.
  expect(volumeSeriesFill('ERROR', 0, false)).toBe('var(--chart-1)')
})

test('level facets lead with errors whatever their count', () => {
  expect(
    bySeverityDesc([
      { value: 'INFO', count: 2485 },
      { value: 'WARN', count: 73 },
      { value: 'DEBUG', count: 38 },
      { value: 'ERROR', count: 34 },
    ]).map((v) => v.value)
  ).toEqual(['ERROR', 'WARN', 'INFO', 'DEBUG'])
})

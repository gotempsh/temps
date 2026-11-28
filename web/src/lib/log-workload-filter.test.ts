// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { workloadFilter } from './log-workload-filter'

describe('workloadFilter', () => {
  test('a kind and a numeric id make a filter', () => {
    expect(workloadFilter('sandbox', '42')).toEqual({ kind: 'sandbox', id: 42 })
  })

  test('half a filter, or a malformed one, is no filter', () => {
    expect(workloadFilter(undefined, '42')).toBeUndefined()
    expect(workloadFilter('sandbox', undefined)).toBeUndefined()
    expect(workloadFilter('sandbox', '4x')).toBeUndefined()
    expect(workloadFilter('Sandbox; drop', '1')).toBeUndefined()
    expect(workloadFilter('sandbox', '99999999999999999999')).toBeUndefined()
  })
})

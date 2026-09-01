// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { computeExpiryIso, groupPermissionsByCategory } from './index.js'

describe('computeExpiryIso', () => {
  test('advances by the given number of days from the reference date', () => {
    const from = new Date('2026-01-01T00:00:00.000Z')
    expect(computeExpiryIso('30', from)).toBe('2026-01-31T00:00:00.000Z')
  })

  test('rejects non-numeric input rather than producing an invalid date', () => {
    expect(computeExpiryIso('never')).toBeNull()
  })

  test('rejects zero and negative day counts', () => {
    expect(computeExpiryIso('0')).toBeNull()
    expect(computeExpiryIso('-5')).toBeNull()
  })
})

describe('groupPermissionsByCategory', () => {
  test('buckets permissions under their category', () => {
    const permissions = [
      { name: 'a', category: 'Deploy' },
      { name: 'b', category: 'Deploy' },
      { name: 'c', category: 'Monitor' },
    ]
    const grouped = groupPermissionsByCategory(permissions)
    expect([...grouped.keys()]).toEqual(['Deploy', 'Monitor'])
    expect(grouped.get('Deploy')?.map((p) => p.name)).toEqual(['a', 'b'])
    expect(grouped.get('Monitor')?.map((p) => p.name)).toEqual(['c'])
  })

  test('falls back permissions with no category into "Other"', () => {
    // The server may not tag every permission with a category — losing
    // ungrouped permissions from the printed list would hide them entirely.
    const permissions = [{ name: 'legacy', category: null }, { name: 'legacy2' }]
    const grouped = groupPermissionsByCategory(permissions)
    expect(grouped.get('Other')?.map((p) => p.name)).toEqual(['legacy', 'legacy2'])
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { sortDomains } from './domain-sort'

const domain = (
  id: number,
  expiration_time?: number | null,
  name = `domain-${id}`
) => ({ id, domain: name, status: 'active', expiration_time })

describe('domain table sorting', () => {
  test('prioritizes expired certificates then nearest expiry before pagination', () => {
    const rows = Array.from({ length: 25 }, (_, i) => domain(i, 1000 + i))
    rows.push(domain(30, -100), domain(31, 1))
    expect(
      sortDomains(rows, 'expiration', 'asc')
        .slice(0, 2)
        .map((row) => row.id)
    ).toEqual([30, 31])
    expect(rows[0].id).toBe(0)
  })
  test('unknown and invalid expiry dates stay last in either direction', () => {
    const rows = [
      domain(1, null),
      domain(2, 200),
      domain(3),
      domain(4, 100),
      domain(5, NaN),
    ]
    expect(sortDomains(rows, 'expiration', 'asc').map((row) => row.id)).toEqual(
      [4, 2, 1, 3, 5]
    )
    expect(
      sortDomains(rows, 'expiration', 'desc').map((row) => row.id)
    ).toEqual([2, 4, 1, 3, 5])
  })
  test('sorts domain names both ways', () => {
    const rows = [domain(1, null, 'z.example'), domain(2, null, 'a.example')]
    expect(sortDomains(rows, 'domain', 'asc')[0].id).toBe(2)
    expect(sortDomains(rows, 'domain', 'desc')[0].id).toBe(1)
  })
  test('sorts status and resolves matching expirations by name', () => {
    const rows = [
      { ...domain(1, 100, 'z.example'), status: 'failed' },
      domain(2, 100, 'a.example'),
    ]
    expect(sortDomains(rows, 'status', 'asc')[0].id).toBe(2)
    expect(sortDomains(rows, 'status', 'desc')[0].id).toBe(1)
    expect(sortDomains(rows, 'expiration', 'desc')[0].id).toBe(2)
  })
  test('handles an empty list', () => {
    expect(sortDomains([], 'expiration', 'asc')).toEqual([])
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import {
  findScanVulnerability,
  findVulnerabilityInPages,
  scanVulnerabilities,
  VulnerabilitySearchIncompleteError,
} from './scan-vulnerabilities'
const row = {
  id: 1,
  scan_id: 1,
  created_at: '2026-09-10T00:00:00Z',
  installed_version: '1.0.0',
  title: 'Example vulnerability',
  vulnerability_id: 'CVE-2026-1234',
  package_name: 'example',
  severity: 'HIGH',
}
test('unwraps the deployed paginated response', () => {
  expect(
    scanVulnerabilities({ data: [row], total: 1, page: 1, page_size: 1000 })
  ).toEqual([row])
})
test('accepts legacy arrays and genuinely empty results', () => {
  expect(scanVulnerabilities([row])).toEqual([row])
  expect(scanVulnerabilities({ data: [], total: 0 })).toEqual([])
})
test('rejects malformed payloads rather than reporting zero vulnerabilities', () => {
  for (const response of [
    null,
    { detail: 'Not found' },
    { data: {} },
    { data: [null] },
    { data: [{}] },
  ]) {
    expect(() => scanVulnerabilities(response)).toThrow(
      'invalid vulnerability list'
    )
  }
})

test('findScanVulnerability treats unknown or malformed ids as not found', () => {
  expect(findScanVulnerability([row], '1')?.id).toBe(1)
  for (const id of [undefined, '', 'abc', '1.5', '-1', '0x1', '2']) {
    expect(findScanVulnerability([row], id)).toBeUndefined()
  }
})

describe('findVulnerabilityInPages', () => {
  const makeRow = (id: number) => ({
    ...row,
    id,
    vulnerability_id: `CVE-${id}`,
  })
  // A scan with 250 findings served 100 at a time, like the server does.
  const all = Array.from({ length: 250 }, (_, i) => makeRow(i + 1))
  const pagesRequested: number[] = []
  const fetchPage = async (page: number) => {
    pagesRequested.push(page)
    return {
      data: all.slice((page - 1) * 100, page * 100),
      total: all.length,
      page,
      page_size: 100,
    }
  }

  test('finds a finding that is not on the first page', async () => {
    pagesRequested.length = 0
    const found = await findVulnerabilityInPages({ vulnId: '230', fetchPage })
    expect(found?.id).toBe(230)
    expect(pagesRequested).toEqual([1, 2, 3])
  })

  test('stops at the first page when the finding is there', async () => {
    pagesRequested.length = 0
    const found = await findVulnerabilityInPages({ vulnId: '7', fetchPage })
    expect(found?.id).toBe(7)
    expect(pagesRequested).toEqual([1])
  })

  test('reports an absent id only after the last page', async () => {
    pagesRequested.length = 0
    const found = await findVulnerabilityInPages({ vulnId: '9999', fetchPage })
    expect(found).toBeUndefined()
    expect(pagesRequested).toEqual([1, 2, 3])
  })

  test('does not fetch for a malformed id', async () => {
    pagesRequested.length = 0
    for (const id of [undefined, '', 'abc', '1.5', '-1']) {
      expect(await findVulnerabilityInPages({ vulnId: id, fetchPage })).toBe(
        undefined
      )
    }
    expect(pagesRequested).toEqual([])
  })

  test('an empty scan ends on the first page', async () => {
    const calls: number[] = []
    const found = await findVulnerabilityInPages({
      vulnId: '1',
      fetchPage: async (page) => {
        calls.push(page)
        return { data: [], total: 0, page, page_size: 100 }
      },
    })
    expect(found).toBeUndefined()
    expect(calls).toEqual([1])
  })

  test('a failing page is an error, not "not found"', async () => {
    const failing = async (page: number) => {
      if (page === 2) throw new Error('boom')
      return { data: all.slice(0, 100), total: 250, page, page_size: 100 }
    }
    await expect(
      findVulnerabilityInPages({ vulnId: '150', fetchPage: failing })
    ).rejects.toThrow('boom')
  })

  test('a server that returns shorter pages than asked for does not end the walk', async () => {
    // Asked for 100 per page, served 40: only `total` says there is more.
    const calls: number[] = []
    const found = await findVulnerabilityInPages({
      vulnId: '200',
      fetchPage: async (page) => {
        calls.push(page)
        return {
          data: all.slice((page - 1) * 40, page * 40),
          total: all.length,
          page,
          page_size: 40,
        }
      },
    })
    expect(found?.id).toBe(200)
    expect(calls).toEqual([1, 2, 3, 4, 5])
  })

  test('without a total, a page shorter than requested ends the walk', async () => {
    const calls: number[] = []
    const found = await findVulnerabilityInPages({
      vulnId: '999',
      pageSize: 100,
      fetchPage: async (page) => {
        calls.push(page)
        return { data: all.slice((page - 1) * 100, page * 100) }
      },
    })
    expect(found).toBeUndefined()
    expect(calls).toEqual([1, 2, 3])
  })

  test('giving up at the page limit is an incomplete search, not "not found"', async () => {
    const big = async (page: number) => ({
      data: Array.from({ length: 100 }, (_, i) => makeRow(page * 1000 + i)),
      total: 1_000_000,
      page,
      page_size: 100,
    })
    const search = findVulnerabilityInPages({
      vulnId: '5',
      fetchPage: big,
      maxPages: 3,
    })
    await expect(search).rejects.toBeInstanceOf(
      VulnerabilitySearchIncompleteError
    )
    await expect(search).rejects.toThrow('first 3 pages')
  })

  test('stops before requesting another page once cancelled', async () => {
    const controller = new AbortController()
    const calls: number[] = []
    const search = findVulnerabilityInPages({
      vulnId: '9999',
      signal: controller.signal,
      fetchPage: async (page, signal) => {
        calls.push(page)
        // The user leaves the page while the second request is in flight.
        if (page === 2) controller.abort()
        expect(signal).toBe(controller.signal)
        return { data: all.slice((page - 1) * 100, page * 100), total: 250 }
      },
    })
    await expect(search).rejects.toThrow()
    expect(calls).toEqual([1, 2])
  })
})

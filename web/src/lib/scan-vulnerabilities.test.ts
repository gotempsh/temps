// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  findScanVulnerability,
  scanVulnerabilities,
  vulnerabilityDetailState,
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

test('the detail page can search the normalised envelope without crashing', () => {
  const envelope = { data: [row], total: 1, page: 1, page_size: 1000 }
  // The raw envelope has no `.find`; this is the crash the page used to hit.
  expect((envelope as unknown as unknown[]).find).toBeUndefined()
  expect(findScanVulnerability(scanVulnerabilities(envelope), '1')).toEqual(row)
})

test('findScanVulnerability treats unknown or malformed ids as not found', () => {
  const rows = [row, { ...row, id: 7 }]
  expect(findScanVulnerability(rows, '7')?.id).toBe(7)
  for (const id of [undefined, '', '0', '2', 'abc', '1.5', '-1', '1abc']) {
    expect(findScanVulnerability(rows, id)).toBeUndefined()
  }
  expect(findScanVulnerability([], '1')).toBeUndefined()
})

const settled = {
  scanError: null,
  vulnerabilitiesError: null,
  isLoading: false,
  vulnerability: row,
}
const notFoundProblem = { title: 'Not Found', status: 404, detail: 'gone' }
const serverProblem = { title: 'Internal Server Error', status: 500 }

test('vulnerabilityDetailState distinguishes ready, loading, not-found and failed', () => {
  expect(vulnerabilityDetailState(settled)).toBe('ready')
  expect(
    vulnerabilityDetailState({
      ...settled,
      isLoading: true,
      vulnerability: undefined,
    })
  ).toBe('loading')
  // Unknown vulnerability id in a known scan: the list settled without it.
  expect(
    vulnerabilityDetailState({ ...settled, vulnerability: undefined })
  ).toBe('not-found')
  // Unknown scan id: the scan read answers 404.
  expect(
    vulnerabilityDetailState({
      ...settled,
      scanError: notFoundProblem,
      vulnerability: undefined,
    })
  ).toBe('not-found')
  expect(
    vulnerabilityDetailState({
      ...settled,
      vulnerabilitiesError: notFoundProblem,
      vulnerability: undefined,
    })
  ).toBe('not-found')
})

test('a failed list read is never reported as not found', () => {
  expect(
    vulnerabilityDetailState({
      ...settled,
      vulnerabilitiesError: serverProblem,
      vulnerability: undefined,
    })
  ).toBe('failed')
  expect(
    vulnerabilityDetailState({
      ...settled,
      vulnerabilitiesError: new TypeError('Failed to fetch'),
      vulnerability: undefined,
    })
  ).toBe('failed')
})

test('a failed scan-context read does not hide a found vulnerability', () => {
  expect(
    vulnerabilityDetailState({ ...settled, scanError: serverProblem })
  ).toBe('ready')
})

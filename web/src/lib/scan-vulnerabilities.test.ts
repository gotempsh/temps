// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { scanVulnerabilities } from './scan-vulnerabilities'
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

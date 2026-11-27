// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { HostFacts } from '@/api/client/types.gen'
import { hostRows } from './host'

const GB = 1024 ** 3
const facts = (over: Partial<HostFacts> = {}): HostFacts => ({
  hostname: 'box-01',
  cpu_cores: 2,
  memory: { total_bytes: 4 * GB, available_bytes: 3 * GB },
  swap_total_bytes: 0,
  disk: { mount_point: '/', total_bytes: 80 * GB, available_bytes: 60 * GB },
  docker: { running: true, version: '27.3.1' },
  ports: [
    { port: 80, holder: 'free' },
    { port: 443, holder: 'free' },
  ],
  lan_ip: '192.168.1.20',
  public_ip: '203.0.113.7',
  tailscale: { running: false, magic_dns: false },
  inbound: { checked: false, note: 'Not checked from outside.' },
  ...over,
})
const row = (rows: ReturnType<typeof hostRows>, label: string) =>
  rows.find((r) => r.label === label)

describe('hostRows', () => {
  test('a healthy box reads plainly, and missing swap is explained', () => {
    const rows = hostRows(facts())
    expect(row(rows, 'Docker')?.value).toBe('27.3.1, running')
    expect(row(rows, 'Ports 80 and 443')?.value).toBe('free on this box')
    expect(row(rows, 'From the internet')?.note).toBe(
      'Not checked from outside.'
    )
    expect(row(rows, 'Swap')?.note).toMatch(/memory spike/)
    expect(rows.some((r) => r.tone)).toBe(false)
  })

  test('problems carry a tone and the fix', () => {
    const rows = hostRows(
      facts({
        memory: { total_bytes: 1 * GB, available_bytes: GB / 2 },
        docker: { running: false, error: 'Docker is not running' },
        ports: [
          { port: 80, holder: 'other' },
          { port: 443, holder: 'free' },
        ],
      })
    )
    expect(row(rows, 'Memory')?.tone).toBe('warning')
    expect(row(rows, 'Docker')).toMatchObject({
      tone: 'error',
      note: 'Docker is not running',
    })
    expect(row(rows, 'Ports 80 and 443')?.value).toBe(
      '80 in use by another program'
    )
  })

  test('Tailscale shows the address and the tailnet name', () => {
    const rows = hostRows(
      facts({
        tailscale: {
          running: true,
          ip: '100.101.102.103',
          machine: 'box-01',
          tailnet: 'tail0000.ts.net',
          magic_dns: true,
        },
      })
    )
    expect(row(rows, 'Tailscale')?.value).toBe(
      'running · 100.101.102.103 · box-01.tail0000.ts.net'
    )
  })
})

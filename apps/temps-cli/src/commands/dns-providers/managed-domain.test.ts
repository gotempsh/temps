// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerDnsProvidersCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('dns-provider domains add', () => {
  test('sends the generated-hostname settings', async () => {
    const run = await runWithApi(
      registerDnsProvidersCommands,
      ['dns-provider', 'domains', 'add', '--id', '2', '-d', 'example.com', '--generated-hostname-mode', 'flat', '--sync-generated-records', '--no-proxied-by-default'],
      { 'POST /dns-providers/2/domains': { id: 1, domain: 'example.com' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({
      domain: 'example.com',
      generated_hostname_mode: 'flat',
      sync_generated_records: true,
      proxied_by_default: false,
    })
    const bad = await runWithApi(registerDnsProvidersCommands, ['dns-provider', 'domains', 'add', '--id', '2', '-d', 'example.com', '--generated-hostname-mode', 'deep'], {})
    expect(bad.error?.message).toMatch(/--generated-hostname-mode must be one of standard, flat/)
    expect(bad.requests).toEqual([])
  })
})

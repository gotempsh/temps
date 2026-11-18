// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { chooseDns01Route, waitForTxt, type ProviderZones } from './dns01.js'

function provider(
  id: number,
  zones: Array<{ domain: string; verified?: boolean; auto_manage?: boolean }>,
  is_active = true,
): ProviderZones {
  return {
    provider: { id, name: `provider-${id}`, is_active },
    zones: zones.map((z) => ({ domain: z.domain, verified: z.verified ?? true, auto_manage: z.auto_manage ?? true })),
  }
}

describe('chooseDns01Route', () => {
  test('uses the provider that manages the zone', () => {
    expect(chooseDns01Route('app.example.com', [provider(4, [{ domain: 'example.com' }])])).toEqual({
      mode: 'provider',
      providerId: 4,
      providerName: 'provider-4',
      zone: 'example.com',
    })
  })

  test('covers the apex and a wildcard under the zone', () => {
    const providers = [provider(4, [{ domain: 'Example.com.' }])]
    expect(chooseDns01Route('example.com', providers).mode).toBe('provider')
    expect(chooseDns01Route('*.example.com', providers).mode).toBe('provider')
  })

  test('the most specific zone wins', () => {
    const route = chooseDns01Route('app.eu.example.com', [
      provider(1, [{ domain: 'example.com' }]),
      provider(2, [{ domain: 'eu.example.com' }]),
    ])
    expect(route).toMatchObject({ mode: 'provider', providerId: 2 })
  })

  test('falls back to manual instructions when no provider manages the zone', () => {
    const route = chooseDns01Route('app.example.org', [provider(1, [{ domain: 'example.com' }])])
    expect(route.mode).toBe('manual')
    expect(route.mode === 'manual' && route.reason).toContain('app.example.org')
  })

  test('a name that only ends with the zone text is not covered', () => {
    expect(chooseDns01Route('app.notexample.com', [provider(1, [{ domain: 'example.com' }])]).mode).toBe('manual')
  })

  test('ignores unverified zones, zones without automatic DNS and inactive providers', () => {
    expect(chooseDns01Route('app.example.com', [provider(1, [{ domain: 'example.com', verified: false }])]).mode).toBe('manual')
    expect(chooseDns01Route('app.example.com', [provider(1, [{ domain: 'example.com', auto_manage: false }])]).mode).toBe('manual')
    expect(chooseDns01Route('app.example.com', [provider(1, [{ domain: 'example.com' }], false)]).mode).toBe('manual')
  })

  test('two providers for the same zone are ambiguous, never guessed', () => {
    const route = chooseDns01Route('app.example.com', [
      provider(1, [{ domain: 'example.com' }]),
      provider(2, [{ domain: 'example.com' }]),
    ])
    expect(route.mode).toBe('manual')
    expect(route.mode === 'manual' && route.reason).toContain('More than one')
  })
})

describe('waitForTxt', () => {
  test('returns once every value is visible', async () => {
    let calls = 0
    const visible = await waitForTxt('_acme-challenge.example.com', ['a', 'b'], {
      lookup: async () => (++calls >= 3 ? ['b', 'a', 'other'] : ['a']),
      sleep: async () => {},
      timeoutMs: 60_000,
    })
    expect(visible).toBe(true)
    expect(calls).toBe(3)
  })

  test('gives up at the deadline without the record', async () => {
    let clock = 0
    const visible = await waitForTxt('_acme-challenge.example.com', ['a'], {
      lookup: async () => [],
      now: () => clock,
      sleep: async (ms) => {
        clock += ms
      },
      timeoutMs: 20_000,
      intervalMs: 5_000,
    })
    expect(visible).toBe(false)
  })

  test('nothing to wait for is never reported visible', async () => {
    expect(await waitForTxt('_acme-challenge.example.com', [], { lookup: async () => ['a'] })).toBe(false)
  })
})

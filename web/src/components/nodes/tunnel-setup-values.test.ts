// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ProviderInfo } from '@/api/client/types.gen'
import {
  providerHostSuffix,
  providerValues,
  type SetupAnswers,
} from '@/components/nodes/tunnel-setup-values'

const cloudflare: ProviderInfo = {
  kind: 'cloudflare_tunnel',
  name: 'Cloudflare Tunnel',
  summary: 'A tunnel.',
  dns: 'automatic',
  needs_public_ip: false,
  works_behind_cgnat: true,
  requirements: [],
  fields: [
    {
      key: 'dns_provider_id',
      label: 'Cloudflare account',
      required: true,
      kind: { type: 'dns_provider', provider_type: 'cloudflare' },
    },
    {
      key: 'domain',
      label: 'Domain',
      required: true,
      kind: {
        type: 'managed_domain',
        provider_field: 'dns_provider_id',
        allow_subdomain: true,
      },
    },
  ],
}

function answers(
  subdomain: string,
  layout: 'suffix' | 'subdomain'
): SetupAnswers {
  return {
    values: { dns_provider_id: '3' },
    picks: { domain: { zone: 'example.com', subdomain, layout } },
  }
}

describe('tunnel setup values', () => {
  test('a one-level label keeps the zone and becomes the host suffix', () => {
    const picked = answers(' Lab ', 'suffix')
    expect(providerValues(cloudflare, picked)).toEqual({
      dns_provider_id: '3',
      domain: 'example.com',
    })
    expect(providerHostSuffix(cloudflare, picked)).toBe('lab')
  })

  test('a subdomain label narrows the domain and has no suffix', () => {
    const picked = answers('lab', 'subdomain')
    expect(providerValues(cloudflare, picked).domain).toBe('lab.example.com')
    expect(providerHostSuffix(cloudflare, picked)).toBeNull()
  })

  test('no label serves the whole zone with no suffix', () => {
    for (const layout of ['suffix', 'subdomain'] as const) {
      const picked = answers('  ', layout)
      expect(providerValues(cloudflare, picked).domain).toBe('example.com')
      expect(providerHostSuffix(cloudflare, picked)).toBeNull()
    }
  })
})

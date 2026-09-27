// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { discoverableDomainOptions } from './email-domain-options'

describe('discoverableDomainOptions', () => {
  test('keys options by provider identity and keeps plain labels for unique names', () => {
    const options = discoverableDomainOptions([
      {
        domain: 'send.example.com',
        provider_identity_id: 'aaaaaaaa-0000-0000-0000-000000000001',
        status: 'verified',
      },
    ])

    expect(options).toEqual([
      {
        value: 'aaaaaaaa-0000-0000-0000-000000000001',
        label: 'send.example.com',
        keywords: 'verified',
      },
    ])
  })

  test('two identities for one name get distinct values and labels', () => {
    const options = discoverableDomainOptions([
      {
        domain: 'send.example.com',
        provider_identity_id: 'aaaaaaaa-0000-0000-0000-000000000001',
        status: 'not_started',
      },
      {
        domain: 'send.example.com',
        provider_identity_id: 'bbbbbbbb-0000-0000-0000-000000000002',
        status: 'not_started',
      },
    ])

    expect(new Set(options.map((o) => o.value)).size).toBe(2)
    expect(new Set(options.map((o) => o.label)).size).toBe(2)
    expect(options[0].label).toBe('send.example.com (not started, aaaaaaaa)')
  })
})

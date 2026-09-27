// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  discoverableDomainOptions,
  selectionAfterProviderChange,
} from './email-domain-options'

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

  test('same-name identities never collapse, even with identical status', () => {
    const domains = ['1', '2', '3'].map((n) => ({
      domain: 'send.example.com',
      provider_identity_id: `${n.repeat(8)}-0000-0000-0000-00000000000${n}`,
      status: 'verified',
    }))

    const options = discoverableDomainOptions(domains)

    expect(options.map((o) => o.value)).toEqual(
      domains.map((d) => d.provider_identity_id)
    )
    expect(options.every((o) => o.value !== 'send.example.com')).toBe(true)
    expect(new Set(options.map((o) => o.label)).size).toBe(3)
  })
})

describe('selectionAfterProviderChange', () => {
  const picked = {
    domain: 'send.example.com',
    providerIdentityId: 'aaaaaaaa-0000-0000-0000-000000000001',
  }

  test('switching provider while importing clears the identity and its domain', () => {
    expect(selectionAfterProviderChange('import', 1, 2, picked)).toEqual({
      domain: '',
      providerIdentityId: '',
    })
  })

  test('switching provider in create mode keeps the typed domain', () => {
    expect(selectionAfterProviderChange('create', 1, 2, picked)).toEqual({
      domain: 'send.example.com',
      providerIdentityId: '',
    })
  })

  test('choosing the first provider keeps a domain typed before it', () => {
    expect(
      selectionAfterProviderChange('import', undefined, 1, {
        domain: 'send.example.com',
        providerIdentityId: '',
      })
    ).toEqual({ domain: 'send.example.com', providerIdentityId: '' })
  })

  test('re-selecting the same provider keeps the selection', () => {
    expect(selectionAfterProviderChange('import', 1, 1, picked)).toBe(picked)
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  primaryEnvironmentUrl,
  reachabilityWarningLines,
} from './reachability-warning'

describe('reachabilityWarningLines', () => {
  test('matches the block the server writes to the deploy log', () => {
    expect(
      reachabilityWarningLines({
        environment_id: 4,
        environment: 'production',
        headline: 'Deployed, but nobody can reach production yet',
        reason: 'app-production.localho.st only resolves on this machine.',
        fixes: [
          {
            kind: 'preview_domain',
            label: 'Set a wildcard preview domain in Settings → Platform',
            href: '/settings#preview-domain',
          },
          {
            kind: 'worker_ingress',
            label: 'Let a worker take public traffic',
            command: 'temps agent ingress delegated',
          },
        ],
      }),
    ).toEqual([
      '⚠ Deployed, but nobody can reach production yet:',
      '  app-production.localho.st only resolves on this machine.',
      '  Make it reachable:',
      '    • set a wildcard preview domain in Settings → Platform',
      '    • or let a worker take public traffic: temps agent ingress delegated',
    ])
  })
})

describe('primaryEnvironmentUrl', () => {
  test('prefers the primary address the server names', () => {
    expect(
      primaryEnvironmentUrl({
        domains: ['http://app.localho.st', 'https://shop.example.com'],
        primary_url: 'https://shop.example.com',
      }),
    ).toBe('https://shop.example.com')
  })

  test('falls back to the first domain, adding a scheme to a bare host', () => {
    expect(primaryEnvironmentUrl({ domains: ['shop.example.com'] })).toBe(
      'https://shop.example.com',
    )
  })

  test('has no link when nobody can reach the environment', () => {
    expect(
      primaryEnvironmentUrl({
        domains: ['http://app.localho.st'],
        no_address_reason: 'only resolves on this machine',
      }),
    ).toBeNull()
  })
})

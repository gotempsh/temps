// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  buildSecretReferencePayload,
  containsLikelyCredential,
} from './ai-first-security'

describe('AI-first secret boundary', () => {
  it('detects credentials before they enter a chat message', () => {
    expect(containsLikelyCredential('API_KEY=sk_live_1234567890abcdef')).toBe(
      true
    )
    expect(
      containsLikelyCredential(
        'postgresql://deploy-user:super-secret-password@db:5432/app'
      )
    ).toBe(true)
    expect(
      containsLikelyCredential('Deploy the main branch to production')
    ).toBe(false)
  })

  it('builds model-visible references without retaining secret values', () => {
    const plaintext = 'sk_live_this_must_never_reach_the_model'
    const payload = buildSecretReferencePayload('northstar', [
      {
        key: 'stripe-secret-key',
        value: plaintext,
        scope: 'production',
      },
    ])

    expect(payload).toEqual([
      {
        key: 'STRIPE_SECRET_KEY',
        reference: 'secret://projects/northstar/production/STRIPE_SECRET_KEY',
        scope: 'production',
        status: 'stored',
      },
    ])
    expect(JSON.stringify(payload)).not.toContain(plaintext)
    expect('value' in payload[0]).toBe(false)
  })
})

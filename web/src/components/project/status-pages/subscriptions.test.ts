// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { SubscriptionSettingsResponse } from '@/api/client'
import {
  pageAfterRemoval,
  senderAfterRefetch,
  senderDomainProblem,
  settingsRequest,
  subscriptionBlockers,
  subscriptionSettingsSchema,
  switchRequest,
} from './subscriptions'

function settings(
  overrides: Partial<SubscriptionSettingsResponse> = {}
): SubscriptionSettingsResponse {
  return {
    status_page_id: 1,
    enabled: false,
    enabled_at: null,
    sender: null,
    email: { configured: true, reason: null, setup_path: null },
    links: { configured: true, reason: null, setup_path: null },
    verified_domains: ['example.test'],
    confirmed_count: 0,
    pending_count: 0,
    max_subscribers: 10000,
    ...overrides,
  }
}

describe('subscriptionBlockers', () => {
  it('is empty when mail and links work', () => {
    expect(subscriptionBlockers(settings())).toEqual([])
  })

  it('names each missing piece with where to fix it', () => {
    const blockers = subscriptionBlockers(
      settings({
        email: {
          configured: false,
          reason: 'No email provider is configured',
          setup_path: '/email',
        },
        links: {
          configured: false,
          reason: 'Temps has no public URL',
          setup_path: '/settings',
        },
      })
    )
    expect(blockers).toEqual([
      { reason: 'No email provider is configured', setupPath: '/email' },
      { reason: 'Temps has no public URL', setupPath: '/settings' },
    ])
  })
})

describe('senderDomainProblem', () => {
  it('accepts an address at a verified domain, any case', () => {
    expect(senderDomainProblem('Status@Example.TEST', ['example.test'])).toBe(
      null
    )
  })

  it('names the domains to use otherwise', () => {
    expect(senderDomainProblem('a@other.test', ['example.test'])).toBe(
      'Use an address at example.test'
    )
  })

  it('says nothing while no domain is verified or the field is empty', () => {
    expect(senderDomainProblem('a@other.test', [])).toBe(null)
    expect(senderDomainProblem('', ['example.test'])).toBe(null)
  })
})

describe('subscriptionSettingsSchema', () => {
  it('allows an empty sender and rejects a non-address', () => {
    expect(subscriptionSettingsSchema.safeParse({ sender: '' }).success).toBe(
      true
    )
    expect(
      subscriptionSettingsSchema.safeParse({ sender: 'not-an-address' }).success
    ).toBe(false)
  })

  it('sends a trimmed sender', () => {
    expect(settingsRequest({ sender: ' s@example.test ' }, true)).toEqual({
      enabled: true,
      sender: 's@example.test',
    })
  })
})

describe('senderAfterRefetch', () => {
  it('adopts the stored sender while the field is untouched', () => {
    expect(senderAfterRefetch('status@example.com', false)).toBe(
      'status@example.com'
    )
    expect(senderAfterRefetch(null, false)).toBe('')
  })

  it('never replaces an address the user is typing', () => {
    expect(senderAfterRefetch('status@example.com', true)).toBeNull()
  })
})

describe('switchRequest', () => {
  it('switches off without the sender, even an invalid edit', () => {
    expect(switchRequest(false, { sender: 'not an address' })).toEqual({
      enabled: false,
    })
  })

  it('switches on with the trimmed sender', () => {
    expect(switchRequest(true, { sender: ' status@example.test ' })).toEqual({
      enabled: true,
      sender: 'status@example.test',
    })
  })
})

describe('pageAfterRemoval', () => {
  it('moves back when the last row of the last page is deleted', () => {
    expect(pageAfterRemoval(2, 21, 20)).toBe(1)
  })

  it('stays on a page that still has rows', () => {
    expect(pageAfterRemoval(2, 22, 20)).toBe(2)
    expect(pageAfterRemoval(1, 5, 20)).toBe(1)
  })

  it('never goes below the first page', () => {
    expect(pageAfterRemoval(1, 1, 20)).toBe(1)
    expect(pageAfterRemoval(3, 0, 20)).toBe(1)
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { SubscriptionSettingsResponse } from '../../api/types.gen.js'
import { settingsBody, subscriptionBlockerLines } from './subscribers.js'

const ready = { configured: true, reason: null, setup_path: null }

function settings(overrides: Partial<SubscriptionSettingsResponse> = {}): SubscriptionSettingsResponse {
  return {
    status_page_id: 3,
    enabled: false,
    enabled_at: null,
    sender: null,
    email: ready,
    links: ready,
    verified_domains: [],
    confirmed_count: 0,
    pending_count: 0,
    max_subscribers: 10000,
    ...overrides,
  }
}

describe('subscriptionBlockerLines', () => {
  it('says what is missing and where to fix it', () => {
    expect(subscriptionBlockerLines(settings({
      email: { configured: false, reason: 'No email provider is configured', setup_path: '/email' },
    }))).toEqual(['No email provider is configured (set it up in the console at /email)'])
  })

  it('is empty when everything is in place', () => {
    expect(subscriptionBlockerLines(settings())).toEqual([])
  })
})

describe('settingsBody', () => {
  it('builds the body from the flags', () => {
    expect(settingsBody({ enable: true, sender: 's@example.test' })).toEqual({
      enabled: true,
      sender: 's@example.test',
    })
    expect(settingsBody({ disable: true })).toEqual({ enabled: false })
    expect(settingsBody({ sender: '' })).toEqual({ sender: '' })
  })

  it('refuses contradictory or empty requests', () => {
    expect(typeof settingsBody({ enable: true, disable: true })).toBe('string')
    expect(typeof settingsBody({})).toBe('string')
  })
})

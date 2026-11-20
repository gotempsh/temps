// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { StatusPageWebhookResponse } from '../../api/types.gen.js'
import { deliveryLine, passwordProblem, webhookUpdateBody, webhookUrlProblem } from './webhooks.js'

function webhook(overrides: Partial<StatusPageWebhookResponse> = {}): StatusPageWebhookResponse {
  return {
    id: 1,
    status_page_id: 3,
    url: 'https://hooks.example.test/status',
    enabled: true,
    has_secret: true,
    last_attempt_at: null,
    last_success_at: null,
    last_failure_reason: null,
    created_at: '2026-11-20T10:00:00Z',
    updated_at: '2026-11-20T10:00:00Z',
    ...overrides,
  }
}

describe('webhookUrlProblem', () => {
  it('accepts plain https URLs only', () => {
    expect(webhookUrlProblem('https://hooks.example.test/status')).toBeNull()
    expect(webhookUrlProblem('http://hooks.example.test/')).toContain('https://')
    expect(webhookUrlProblem('https://u:p@hooks.example.test/')).toContain('user name')
    expect(webhookUrlProblem('https://hooks.example.test/#x')).toContain('fragment')
    expect(webhookUrlProblem('nope')).toContain('not a valid URL')
    expect(webhookUrlProblem(' ')).toContain('empty')
  })
})

describe('webhookUpdateBody', () => {
  it('builds the body from the flags', () => {
    expect(webhookUpdateBody({ disable: true })).toEqual({ enabled: false })
    expect(webhookUpdateBody({ url: ' https://hooks.example.test/v2 ', enable: true })).toEqual({
      url: 'https://hooks.example.test/v2',
      enabled: true,
    })
  })

  it('refuses contradictory, invalid or empty requests', () => {
    expect(typeof webhookUpdateBody({ enable: true, disable: true })).toBe('string')
    expect(typeof webhookUpdateBody({ url: 'http://hooks.example.test/' })).toBe('string')
    expect(typeof webhookUpdateBody({})).toBe('string')
  })
})

describe('deliveryLine', () => {
  it('summarizes the last delivery', () => {
    expect(deliveryLine(webhook())).toBe('no event yet')
    expect(deliveryLine(webhook({ enabled: false }))).toBe('paused')
    expect(deliveryLine(webhook({ last_failure_reason: 'the endpoint answered 500' }))).toBe(
      'failing: the endpoint answered 500',
    )
    expect(deliveryLine(webhook({ last_success_at: '2026-11-20T10:05:00Z' }))).toStartWith('delivered')
  })
})

describe('passwordProblem', () => {
  it('matches the server rules', () => {
    expect(passwordProblem('long enough')).toBeNull()
    expect(passwordProblem('short')).toContain('at least 8')
    expect(passwordProblem('x'.repeat(257))).toContain('at most 256')
    expect(passwordProblem('tab\there')).toContain('control')
  })
})

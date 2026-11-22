// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { StatusPageWebhookResponse } from '@/api/client'
import {
  deliveryState,
  pagePasswordSchema,
  webhookUrlProblem,
} from './notifications'

function webhook(
  overrides: Partial<StatusPageWebhookResponse> = {}
): StatusPageWebhookResponse {
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
  it('accepts a plain https URL', () => {
    expect(webhookUrlProblem(' https://hooks.example.test/status ')).toBeNull()
  })

  it('says what is wrong', () => {
    expect(webhookUrlProblem('')).toContain('Enter')
    expect(webhookUrlProblem('hooks.example.test')).toContain('full URL')
    expect(webhookUrlProblem('http://hooks.example.test/')).toContain(
      'https://'
    )
    expect(webhookUrlProblem('https://u:p@hooks.example.test/')).toContain(
      'credentials'
    )
    expect(webhookUrlProblem('https://hooks.example.test/#x')).toContain(
      'fragment'
    )
    expect(
      webhookUrlProblem(`https://hooks.example.test/${'a'.repeat(2100)}`)
    ).toContain('2048')
  })
})

describe('deliveryState', () => {
  it('reads the last delivery', () => {
    expect(deliveryState(webhook()).label).toBe('No event yet')
    expect(
      deliveryState(webhook({ last_success_at: '2026-11-20T10:05:00Z' })).tone
    ).toBe('success')
    const failing = deliveryState(
      webhook({ last_failure_reason: 'the endpoint answered 500' })
    )
    expect(failing.tone).toBe('warning')
    expect(failing.detail).toBe('the endpoint answered 500')
    expect(
      deliveryState(webhook({ enabled: false, last_failure_reason: 'x' })).label
    ).toBe('Paused')
  })
})

describe('pagePasswordSchema', () => {
  it('bounds the password', () => {
    expect(
      pagePasswordSchema.safeParse({ password: 'long enough' }).success
    ).toBe(true)
    expect(pagePasswordSchema.safeParse({ password: 'short' }).success).toBe(
      false
    )
    expect(
      pagePasswordSchema.safeParse({ password: 'x'.repeat(257) }).success
    ).toBe(false)
    expect(
      pagePasswordSchema.safeParse({ password: 'tab\tinside' }).success
    ).toBe(false)
  })
})

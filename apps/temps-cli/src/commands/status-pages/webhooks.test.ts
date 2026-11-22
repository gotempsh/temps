// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { StatusPageWebhookResponse } from '../../api/types.gen.js'
import {
  deliveryLine,
  MAX_PASSWORD_STDIN_BYTES,
  passwordProblem,
  readFirstLine,
  webhookUpdateBody,
  webhookUrlProblem,
} from './webhooks.js'

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

describe('readFirstLine', () => {
  async function* chunks(...parts: string[]): AsyncGenerator<string> {
    for (const part of parts) yield part
  }

  it('returns the first line, without its line ending, across chunks', async () => {
    expect(await readFirstLine(chunks('correct ', 'horse\r\nsecond line\n'))).toBe('correct horse')
    expect(await readFirstLine(chunks('no newline at all'))).toBe('no newline at all')
    expect(await readFirstLine(chunks())).toBe('')
  })

  it('stops at the first newline instead of waiting for the stream to end', async () => {
    let pulledAfterNewline = false
    async function* endless(): AsyncGenerator<string> {
      yield 'password\n'
      pulledAfterNewline = true
      for (;;) yield 'x'.repeat(1024)
    }
    expect(await readFirstLine(endless())).toBe('password')
    expect(pulledAfterNewline).toBe(false)
  })

  it('refuses a first line longer than the limit without reading the rest', async () => {
    let pulled = 0
    async function* huge(): AsyncGenerator<string> {
      for (;;) {
        pulled += 1
        yield 'x'.repeat(512)
      }
    }
    expect(await readFirstLine(huge())).toBeNull()
    expect(pulled).toBeLessThanOrEqual(Math.ceil(MAX_PASSWORD_STDIN_BYTES / 512) + 1)
    expect(await readFirstLine(chunks('x'.repeat(10)), 9)).toBeNull()
    expect(await readFirstLine(chunks('x'.repeat(9) + '\nmore'), 9)).toBe('x'.repeat(9))
  })
})

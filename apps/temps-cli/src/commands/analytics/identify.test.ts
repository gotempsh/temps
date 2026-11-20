// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildIdentifyBody, identifyFailure } from './identify'

describe('buildIdentifyBody', () => {
  test('needs a user id', () => {
    const result = buildIdentifyBody({ userId: '  ' })
    expect(result.ok).toBe(false)
  })

  test('sends no traits unless asked', () => {
    expect(buildIdentifyBody({ userId: 'user_1' })).toEqual({ ok: true, body: { user_id: 'user_1' } })
  })

  test('builds traits like enrich: --data, then --set, then --unset', () => {
    expect(
      buildIdentifyBody({
        userId: 'user_1',
        data: '{"plan":"free","seats":3}',
        set: ['plan=pro'],
        unset: ['trial'],
      }),
    ).toEqual({ ok: true, body: { user_id: 'user_1', traits: { plan: 'pro', seats: 3, trial: null } } })
  })

  test('passes on a traits error', () => {
    const result = buildIdentifyBody({ userId: 'user_1', set: ['no-equals'] })
    expect(result.ok).toBe(false)
    if (!result.ok) expect(result.error).toContain('--set "no-equals"')
  })
})

describe('identifyFailure', () => {
  test('is null when the identity was stored or already there', () => {
    expect(identifyFailure('enc_a', { applied: true, outcome: 'updated', message: 'ok' })).toBeNull()
  })

  test('names the visitor and the outcome when nothing was stored', () => {
    expect(
      identifyFailure('enc_a', {
        applied: false,
        outcome: 'cookieless_project',
        message: 'Identify stores nothing in a cookieless project.',
      }),
    ).toBe(
      'Visitor "enc_a" was not identified (cookieless_project): Identify stores nothing in a cookieless project.',
    )
  })
})

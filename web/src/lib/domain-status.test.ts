// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  failedRetryAction,
  isServingCert,
  isTerminalOrderStatus,
} from './domain-status'

describe('failedRetryAction', () => {
  test('an order the CA invalidated needs a new order, not another verify', () => {
    expect(failedRetryAction({ status: 'invalid' })).toBe('new-order')
  })

  test('every terminal order state needs a new order', () => {
    for (const status of ['cancelled', 'canceled', 'revoked', 'deactivated']) {
      expect(failedRetryAction({ status })).toBe('new-order')
    }
  })

  test('a missing order needs a new order', () => {
    expect(failedRetryAction(null)).toBe('new-order')
    expect(failedRetryAction(undefined)).toBe('new-order')
  })

  test('an order that is still open can be verified again', () => {
    expect(failedRetryAction({ status: 'pending' })).toBe('verify')
    expect(failedRetryAction({ status: 'ready' })).toBe('verify')
  })
})

describe('isTerminalOrderStatus', () => {
  test('treats unknown and empty statuses as not terminal', () => {
    expect(isTerminalOrderStatus('processing')).toBe(false)
    expect(isTerminalOrderStatus('')).toBe(false)
    expect(isTerminalOrderStatus(undefined)).toBe(false)
  })
})

describe('isServingCert', () => {
  test('serves active and renewal-failed certificates only', () => {
    expect(isServingCert('active')).toBe(true)
    expect(isServingCert('active_renewal_failed')).toBe(true)
    expect(isServingCert('failed')).toBe(false)
    expect(isServingCert('challenge_requested')).toBe(false)
  })
})

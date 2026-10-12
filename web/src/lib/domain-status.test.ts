// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  DOMAIN_POLL_FAST_MS,
  DOMAIN_POLL_FAST_READS,
  DOMAIN_POLL_SLOW_MS,
  domainRefetchInterval,
  isIssuanceInFlight,
  ORDER_CATCH_UP_WINDOW_MS,
  orderRefetchInterval,
} from './domain-status'

test('every in-flight issuance status polls', () => {
  for (const status of [
    'pending',
    'pending_dns',
    'pending_http',
    'challenge_requested',
    'processing',
    'on_demand_issuing',
  ]) {
    expect(isIssuanceInFlight(status)).toBe(true)
    expect(domainRefetchInterval(status, 0)).toBe(DOMAIN_POLL_FAST_MS)
  }
})

test('settled statuses stop polling', () => {
  for (const status of [
    'active',
    'active_renewal_failed',
    'failed',
    'expired',
    'unknown-future-status',
    '',
    null,
    undefined,
  ]) {
    expect(isIssuanceInFlight(status)).toBe(false)
    expect(domainRefetchInterval(status, 0)).toBe(false)
  }
})

test('polling backs off after the fast reads are used up', () => {
  expect(
    domainRefetchInterval('challenge_requested', DOMAIN_POLL_FAST_READS - 1)
  ).toBe(DOMAIN_POLL_FAST_MS)
  expect(
    domainRefetchInterval('challenge_requested', DOMAIN_POLL_FAST_READS)
  ).toBe(DOMAIN_POLL_SLOW_MS)
})

test('the order follows the domain while issuance is in flight', () => {
  expect(orderRefetchInterval('challenge_requested', 'pending', 0, 0)).toBe(
    DOMAIN_POLL_FAST_MS
  )
  expect(
    orderRefetchInterval(
      'challenge_requested',
      'pending',
      DOMAIN_POLL_FAST_READS,
      0
    )
  ).toBe(DOMAIN_POLL_SLOW_MS)
})

test('a settled domain keeps polling an order that has not settled yet', () => {
  // The domain poll saw `failed` before the order poll saw `invalid`.
  expect(orderRefetchInterval('failed', 'pending', 3, 4_000)).toBe(
    DOMAIN_POLL_FAST_MS
  )
  expect(orderRefetchInterval('failed', 'processing', 3, 4_000)).toBe(
    DOMAIN_POLL_FAST_MS
  )
})

test('a domain that fails after a long issuance still gets its catch-up read', () => {
  // The order query has made far more than the fast reads by then; only the
  // time since the domain settled bounds the catch-up.
  expect(
    orderRefetchInterval('failed', 'pending', DOMAIN_POLL_FAST_READS * 5, 5_000)
  ).toBe(DOMAIN_POLL_FAST_MS)
})

test('the order stops polling once both are settled or the order is unknown', () => {
  expect(orderRefetchInterval('failed', 'invalid', 3, 1_000)).toBe(false)
  expect(orderRefetchInterval('active', 'valid', 3, 1_000)).toBe(false)
  expect(orderRefetchInterval('failed', 'cancelled', 3, 1_000)).toBe(false)
  expect(orderRefetchInterval('failed', undefined, 3, 1_000)).toBe(false)
})

test('an order stuck open behind a settled domain is bounded in time', () => {
  expect(
    orderRefetchInterval('failed', 'pending', 3, ORDER_CATCH_UP_WINDOW_MS)
  ).toBe(false)
})

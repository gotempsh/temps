// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  DOMAIN_POLL_FAST_MS,
  DOMAIN_POLL_FAST_READS,
  DOMAIN_POLL_SLOW_MS,
  domainRefetchInterval,
  isIssuanceInFlight,
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
  expect(domainRefetchInterval('challenge_requested', DOMAIN_POLL_FAST_READS - 1)).toBe(
    DOMAIN_POLL_FAST_MS
  )
  expect(domainRefetchInterval('challenge_requested', DOMAIN_POLL_FAST_READS)).toBe(
    DOMAIN_POLL_SLOW_MS
  )
})

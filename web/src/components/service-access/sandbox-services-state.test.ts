// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  approvalExpirySchema,
  serviceAccessLabel,
} from './sandbox-services-state'
test('approval alone never claims active access', () => {
  expect(serviceAccessLabel('approved', false)).toBe('Connecting')
  expect(serviceAccessLabel('approved', true)).toBe('Active')
  expect(serviceAccessLabel('revoked', true)).toBe('Revoked')
  expect(serviceAccessLabel('pending_approval', false)).toBe(
    'Awaiting approval'
  )
})

test('optional approval expiry accepts lifetime or future dates and rejects invalid and expired input', () => {
  expect(approvalExpirySchema.safeParse({ deadline: '' }).success).toBe(true)
  expect(
    approvalExpirySchema.safeParse({
      deadline: new Date(Date.now() + 3600_000).toISOString(),
    }).success
  ).toBe(true)
  expect(
    approvalExpirySchema.safeParse({ deadline: 'not-a-date' }).success
  ).toBe(false)
  expect(
    approvalExpirySchema.safeParse({ deadline: new Date().toISOString() })
      .success
  ).toBe(false)
})

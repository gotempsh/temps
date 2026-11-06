// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  earliestExpiryDate,
  expiryDateInput,
  expiryForUpdate,
  expiryFromDateInput,
} from './api-key-expiry'

// A key created by `temps login` expires 90 days after sign-in, at that time.
const STORED = '2026-12-05T17:30:00Z'

test('saving a key without touching its expiry keeps the stored timestamp', () => {
  const shown = expiryDateInput(STORED)
  expect(shown).toBe('2026-12-05')
  expect(expiryForUpdate(shown, STORED)).toBe(STORED)
})

test('an expired key saves with its expiry unchanged', () => {
  const expired = '2020-01-01T12:00:00Z'
  expect(expiryForUpdate(expiryDateInput(expired), expired)).toBe(expired)
})

test('a changed or cleared date replaces the stored expiry', () => {
  expect(expiryForUpdate('2027-01-10', STORED)).toBe('2027-01-10T00:00:00.000Z')
  expect(expiryForUpdate('', STORED)).toBeNull()
  expect(expiryForUpdate('2027-01-10', null)).toBe('2027-01-10T00:00:00.000Z')
  expect(expiryForUpdate('', null)).toBeNull()
})

test('the earliest pickable expiry is tomorrow in UTC, never today', () => {
  expect(earliestExpiryDate(new Date('2026-11-05T23:59:00Z'))).toBe(
    '2026-11-06'
  )
  expect(earliestExpiryDate(new Date('2026-12-31T00:00:00Z'))).toBe(
    '2027-01-01'
  )
  // Today's date as an expiry is 00:00 UTC today: already in the past.
  expect(Date.parse(expiryFromDateInput('2026-11-05')!)).toBeLessThan(
    Date.parse('2026-11-05T08:00:00Z')
  )
})

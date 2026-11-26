// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { errorCategoryLabel } from './on-demand-certs'

const deadline = '2026-11-25T12:30:00Z'
const shown = new Date(Date.parse(deadline)).toLocaleString()

test('a rate limit names the ISO 8601 deadline the API sends', () => {
  expect(errorCategoryLabel('rate_limited', null, deadline)).toBe(
    `Rate limit from Let's Encrypt — retry after ${shown}`
  )
})

test('an epoch-millisecond deadline is still shown', () => {
  expect(errorCategoryLabel('rate_limited', null, Date.parse(deadline))).toBe(
    `Rate limit from Let's Encrypt — retry after ${shown}`
  )
})

test('no deadline, or one that cannot be read, adds no retry time', () => {
  for (const value of [null, undefined, '', 'not a date'])
    expect(errorCategoryLabel('rate_limited', null, value)).toBe(
      "Rate limit from Let's Encrypt"
    )
})

test('the other categories keep their messages', () => {
  expect(errorCategoryLabel(null)).toBeNull()
  expect(errorCategoryLabel('internal', '429')).toBe(
    'Internal issuance error (429)'
  )
  expect(errorCategoryLabel('something_new')).toBe('something_new')
})

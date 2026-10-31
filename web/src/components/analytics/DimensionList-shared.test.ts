// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { dimensionRowHref } from './DimensionList-shared'

const range = new URLSearchParams('filter=7days&environment=3')

test('a country row opens the visitors from that country, on the same range', () => {
  expect(dimensionRowHref('shop', 'countries', 'Spain', range)).toBe(
    '/projects/shop/analytics/segments/countries/Spain?filter=7days'
  )
})

test('an event row keeps opening the event page', () => {
  expect(
    dimensionRowHref('shop', 'events', 'signup', new URLSearchParams())
  ).toBe('/projects/shop/analytics/events/signup')
})

test('a value is encoded, and a custom range carries both ends', () => {
  const custom = new URLSearchParams(
    'filter=custom&from=2026-10-01T00:00:00.000Z&to=2026-10-02T00:00:00.000Z'
  )
  expect(
    dimensionRowHref('shop', 'referrers', 'https://example.test/a b', custom)
  ).toBe(
    '/projects/shop/analytics/segments/referrers/https%3A%2F%2Fexample.test%2Fa%20b?filter=custom&from=2026-10-01T00%3A00%3A00.000Z&to=2026-10-02T00%3A00%3A00.000Z'
  )
})

test('pages cannot filter visitors, so their rows open nothing', () => {
  expect(
    dimensionRowHref('shop', 'pages', '/pricing', new URLSearchParams())
  ).toBeUndefined()
})

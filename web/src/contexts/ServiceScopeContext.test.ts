// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { scopedServiceId } from './ServiceScopeContext'

const scope = {
  services: [
    { id: 4, name: 'web', slug: 'web' },
    { id: 9, name: 'api', slug: 'api' },
  ],
}

test('a requested service of the project is kept', () => {
  expect(scopedServiceId(scope, 9)).toBe(9)
  expect(scopedServiceId(scope, 9, { allowAll: true })).toBe(9)
})

test('a service outside the project is never shown', () => {
  expect(scopedServiceId(scope, 1)).toBe(4)
  expect(scopedServiceId(scope, 1, { allowAll: true })).toBeUndefined()
})

test('with no service picked, single-service pages start on the first', () => {
  expect(scopedServiceId(scope, undefined)).toBe(4)
  expect(scopedServiceId(scope, undefined, { allowAll: true })).toBeUndefined()
  expect(scopedServiceId({ services: [] }, undefined)).toBeUndefined()
})

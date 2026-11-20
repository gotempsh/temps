// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, expect, test } from 'bun:test'
import { client } from '@/api/client/client.gen'
import { mockApi, restoreAllMockApis } from './fetch-mock'

const original = client.getConfig().fetch

// Deliberately never calls `restore()`: the preloaded afterEach hook has to.
test('first test installs a mock and forgets to restore it', () => {
  mockApi({})
  expect(client.getConfig().fetch).not.toBe(original)
})

test('the next test starts with the real client again', () => {
  expect(client.getConfig().fetch).toBe(original)
})

test('restoreAllMockApis unwinds nested mocks to the original client', () => {
  mockApi({})
  mockApi({})
  restoreAllMockApis()
  expect(client.getConfig().fetch).toBe(original)
})

test('restore() twice is harmless', () => {
  const api = mockApi({})
  api.restore()
  api.restore()
  expect(client.getConfig().fetch).toBe(original)
})

afterAll(restoreAllMockApis)

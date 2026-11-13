// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { client } from '@/api/client/client.gen'

const original = client.getConfig()

// Deliberately restores the way many tests do, with the config it read: as
// setConfig merges, that alone keeps the fake fetch. The preloaded afterEach
// hook has to put the real transport back, and clear what the test stored.
test('a test leaves a fake fetch, a base URL and stored state behind', () => {
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(async () => Response.json([]), {
      preconnect: fetch.preconnect,
    }),
  })
  localStorage.setItem('temps.ai.activeChat', '{"title":"left behind"}')
  sessionStorage.setItem('left', 'behind')
  client.setConfig(original)
  expect(localStorage.length).toBe(1)
})

test('the next test starts with the real transport and empty storage', () => {
  expect(client.getConfig().fetch).toBe(original.fetch)
  expect(client.getConfig().baseUrl).toBe(original.baseUrl)
  expect(localStorage.length).toBe(0)
  expect(sessionStorage.length).toBe(0)
})

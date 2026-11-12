// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { createClient, createConfig } from '@/api/client/client'
import { restoreClientConfig } from './client-config'

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
})

test('a fake fetch given to the client does not answer after the restore', async () => {
  // A client of its own: the shared one has been through other files' tests.
  const client = createClient(
    createConfig({ baseUrl: 'http://console.example.test/api' })
  )
  const original = client.getConfig()
  client.setConfig({
    fetch: Object.assign(() => Promise.resolve(Response.json('leaked')), {
      preconnect: fetch.preconnect,
    }),
  })
  // What `setConfig(original)` leaves behind: the merge keeps the fake.
  client.setConfig(original)
  expect(client.getConfig().fetch).toBeDefined()

  restoreClientConfig(client, original)
  expect(client.getConfig().fetch).toBeUndefined()
  expect(client.getConfig().baseUrl).toBe('http://console.example.test/api')

  // A later test that fakes the global fetch is the one answered.
  globalThis.fetch = (async () =>
    Response.json('global')) as unknown as typeof fetch
  const { data } = await client.get({ url: '/anything' })
  expect(data).toBe('global')
})

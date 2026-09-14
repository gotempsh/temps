// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { client } from './client/client.gen'
import { updateMonitorPath } from './monitors'

const originalConfig = client.getConfig()
afterEach(() => client.setConfig(originalConfig))

test('PATCH changes an existing monitor path and returns its preserved identity', async () => {
  let request: Request | undefined
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL) => {
        request = input as Request
        return Response.json({
          id: 17,
          project_id: 4,
          check_path: '/ready',
          monitor_url: 'https://app.example.test/ready',
        })
      },
      { preconnect: fetch.preconnect }
    ),
  })
  const updated = await updateMonitorPath(17, '/ready')
  expect(request?.method).toBe('PATCH')
  expect(request?.url).toBe('https://console.example.test/api/monitors/17')
  expect(await request?.json()).toEqual({ check_path: '/ready' })
  expect(updated.id).toBe(17)
  expect(updated.monitor_url).toBe('https://app.example.test/ready')
})

test('a forbidden save rejects instead of appearing successful', async () => {
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async () =>
        Response.json({ title: 'Forbidden', status: 403 }, { status: 403 }),
      { preconnect: fetch.preconnect }
    ),
  })
  await expect(updateMonitorPath(17, '/')).rejects.toMatchObject({
    status: 403,
  })
})

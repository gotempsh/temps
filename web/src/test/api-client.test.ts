// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { afterEach, expect, test } from 'bun:test'
import { client } from '@/api/client/client.gen'
import { restoreApiClient } from './api-client'

const original = client.getConfig()
afterEach(() => restoreApiClient(original))

const fakeFetch = (async () => Response.json({})) as unknown as typeof fetch

test('restoring clears a fetch and base URL the saved config did not have', () => {
  const saved = { ...client.getConfig(), baseUrl: undefined, fetch: undefined }
  client.setConfig({
    baseUrl: 'http://console.example.test/api',
    fetch: fakeFetch,
  })

  restoreApiClient(saved)

  expect(client.getConfig().fetch).toBeUndefined()
  expect(client.getConfig().baseUrl).toBeUndefined()
})

test('restoring puts back a fetch and base URL the saved config had', () => {
  client.setConfig({
    baseUrl: 'http://saved.example.test/api',
    fetch: fakeFetch,
  })
  const saved = client.getConfig()
  client.setConfig({
    baseUrl: 'http://other.example.test/api',
    fetch: (async () => Response.json([])) as unknown as typeof fetch,
  })

  restoreApiClient(saved)

  expect(client.getConfig().fetch).toBe(fakeFetch)
  expect(client.getConfig().baseUrl).toBe('http://saved.example.test/api')
})

test('a plain setConfig of the saved config keeps the fake fetch', () => {
  // Why restoreApiClient exists: setConfig merges.
  // A config that never had a fetch has no `fetch` key at all.
  const { fetch: _unset, ...saved } = client.getConfig()
  client.setConfig({ fetch: fakeFetch })
  client.setConfig(saved)
  expect(client.getConfig().fetch).toBe(fakeFetch)
})

function testFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return testFiles(path)
    return /\.test\.tsx?$/.test(name) ? [path] : []
  })
}

test('no test restores the shared client with a plain setConfig', () => {
  const src = join(import.meta.dir, '..')
  const offenders = testFiles(src).filter(
    (path) =>
      path !== import.meta.path &&
      /\.setConfig\(\s*(original|previous|saved)\w*\s*\)/.test(
        readFileSync(path, 'utf8')
      )
  )
  expect(offenders.map((path) => relative(src, path))).toEqual([])
})

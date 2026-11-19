// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { fileURLToPath } from 'node:url'
import { QueryClient } from '@tanstack/react-query'
import {
  getAiHarnessStatusQueryKey,
  getApiKeyQueryKey,
  listApiKeysQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { invalidateApiKeyQueries } from './api-key-queries'

describe('invalidateApiKeyQueries', () => {
  test('marks the key list and the AI harness status stale', async () => {
    const client = new QueryClient()
    const listKey = listApiKeysQueryKey({ query: { page: 1, page_size: 20 } })
    const statusKey = getAiHarnessStatusQueryKey()
    const unrelatedKey = getApiKeyQueryKey({ path: { id: 7 } })
    client.setQueryData(listKey, { api_keys: [], total: 0 })
    client.setQueryData(statusKey, { status: 'missing' })
    client.setQueryData(unrelatedKey, { id: 7 })

    await invalidateApiKeyQueries(client)

    expect(client.getQueryState(listKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(statusKey)?.isInvalidated).toBe(true)
    expect(client.getQueryState(unrelatedKey)?.isInvalidated).toBe(false)
  })
})

/** Generated-client names that create, change or remove an API key. */
const KEY_MUTATION =
  /\b(createApiKey|deleteApiKey|activateApiKey|deactivateApiKey|updateApiKey|rotateApiKey|cliDeviceApprove)(Mutation)?\b/
const CLIENT_IMPORT =
  /import\s*\{([^}]*)\}\s*from\s*['"]@\/api\/client[^'"]*['"]/g

/** Console modules outside the generated client, test files excluded. */
async function sourceFiles(root: string): Promise<string[]> {
  const files: string[] = []
  for await (const path of new Bun.Glob('**/*.{ts,tsx}').scan(root)) {
    if (path.startsWith('api/') || /\.test\.tsx?$/.test(path)) continue
    files.push(path)
  }
  return files.sort()
}

describe('API key mutations', () => {
  // Every console module that changes an API key must refresh the AI
  // harness status too, or the setup checklist and banner keep a stale state.
  test('every module that changes an API key refreshes the harness status', async () => {
    // fileURLToPath, not `.pathname`: a checkout path with a space or other
    // escaped character would stay percent-encoded and the scan would fail.
    const src = fileURLToPath(new URL('../', import.meta.url))
    const missing: string[] = []
    for (const file of await sourceFiles(src)) {
      const source = await Bun.file(`${src}${file}`).text()
      const changesKeys = [...source.matchAll(CLIENT_IMPORT)].some((match) =>
        KEY_MUTATION.test(match[1])
      )
      if (changesKeys && !source.includes('invalidateApiKeyQueries(')) {
        missing.push(file)
      }
    }
    expect(missing).toEqual([])
  })
})

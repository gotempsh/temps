// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type {
  AnalyticsIngestKey,
  AnalyticsIngestKeyList,
} from '@/api/client/types.gen'
import {
  fetchAllIngestKeys,
  INGEST_KEYS_MAX_PAGES,
  INGEST_KEYS_PAGE_SIZE,
  ingestKeyState,
} from './ingest-keys'

const at = '2026-01-01T00:00:00Z'

function key(id: number, isActive: boolean): AnalyticsIngestKey {
  return {
    id,
    project_id: 3,
    name: `key ${id}`,
    public_key: `pa_${id}`,
    is_active: isActive,
    event_count: 0,
    created_at: at,
    updated_at: at,
  }
}

/** A server holding `keys` (newest first), answering pages of the API size. */
function server(keys: AnalyticsIngestKey[]) {
  const asked: number[] = []
  const fetchPage = async (page: number): Promise<AnalyticsIngestKeyList> => {
    asked.push(page)
    const start = (page - 1) * INGEST_KEYS_PAGE_SIZE
    return {
      items: keys.slice(start, start + INGEST_KEYS_PAGE_SIZE),
      page,
      page_size: INGEST_KEYS_PAGE_SIZE,
      total: keys.length,
    }
  }
  return { asked, fetchPage }
}

test('the active key is found behind a full page of newer revoked keys', async () => {
  // 150 revoked keys created after the one still in use.
  const keys = [
    ...Array.from({ length: 150 }, (_, i) => key(1000 - i, false)),
    key(7, true),
  ]
  const { asked, fetchPage } = server(keys)

  const all = await fetchAllIngestKeys(fetchPage)

  expect(asked).toEqual([1, 2])
  expect(all.keys).toHaveLength(151)
  expect(all.unread).toBe(0)
  const state = ingestKeyState(all.keys)
  expect(state.primaryKey?.public_key).toBe('pa_7')
  expect(state.activeKeys.map((k) => k.id)).toEqual([7])
  expect(state.revokedKeys).toHaveLength(150)
})

test('one short page is one request', async () => {
  const { asked, fetchPage } = server([key(2, true), key(1, false)])
  const all = await fetchAllIngestKeys(fetchPage)
  expect(asked).toEqual([1])
  expect(ingestKeyState(all.keys).primaryKey?.id).toBe(2)
})

test('no keys means no primary key, and the snippets fall back to the placeholder', async () => {
  const { fetchPage } = server([])
  const all = await fetchAllIngestKeys(fetchPage)
  expect(all).toEqual({ keys: [], total: 0, unread: 0 })
  expect(ingestKeyState(all.keys).primaryKey).toBeUndefined()
})

test('a key created while paging is not listed twice', async () => {
  const keys = Array.from({ length: 150 }, (_, i) => key(500 - i, false))
  let created = false
  const fetchPage = async (page: number): Promise<AnalyticsIngestKeyList> => {
    // Between the two requests a new key pushes every row down by one.
    const current = created ? [key(999, true), ...keys] : keys
    created = true
    const start = (page - 1) * INGEST_KEYS_PAGE_SIZE
    return {
      items: current.slice(start, start + INGEST_KEYS_PAGE_SIZE),
      page,
      page_size: INGEST_KEYS_PAGE_SIZE,
      total: current.length,
    }
  }
  const all = await fetchAllIngestKeys(fetchPage)
  const ids = all.keys.map((k) => k.id)
  expect(new Set(ids).size).toBe(ids.length)
})

test('reading stops at the page limit and reports what it did not read', async () => {
  const pages = INGEST_KEYS_MAX_PAGES + 2
  const keys = Array.from({ length: pages * INGEST_KEYS_PAGE_SIZE }, (_, i) =>
    key(100000 - i, false)
  )
  const { asked, fetchPage } = server(keys)
  const all = await fetchAllIngestKeys(fetchPage)
  expect(asked).toHaveLength(INGEST_KEYS_MAX_PAGES)
  expect(all.unread).toBe(2 * INGEST_KEYS_PAGE_SIZE)
})

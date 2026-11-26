// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AnalyticsIngestKey,
  AnalyticsIngestKeyList,
} from '@/api/client/types.gen'

/** The most keys one request returns (the API's maximum page size). */
export const INGEST_KEYS_PAGE_SIZE = 100

/**
 * The most pages read for one project (5,000 keys). Revoked keys are kept, so
 * a project that rotates often accumulates them; past this the card says how
 * many it did not read instead of fetching without end.
 */
export const INGEST_KEYS_MAX_PAGES = 50

export interface AllIngestKeys {
  /** Every key read, newest first, each once. */
  keys: AnalyticsIngestKey[]
  /** How many keys the project has. */
  total: number
  /** Keys past the page limit, not read. */
  unread: number
}

/**
 * Reads a project's keys page by page until it has them all. The list is
 * newest first and keeps revoked keys, so an active key can sit behind a
 * full page of newer revoked ones; reading only the first page would show
 * "No ingest key yet" for a project that has one.
 */
export async function fetchAllIngestKeys(
  fetchPage: (page: number) => Promise<AnalyticsIngestKeyList>
): Promise<AllIngestKeys> {
  const keys: AnalyticsIngestKey[] = []
  const seen = new Set<number>()
  let total = 0
  for (let page = 1; page <= INGEST_KEYS_MAX_PAGES; page++) {
    const list = await fetchPage(page)
    total = list.total
    for (const key of list.items) {
      // A key created while paging shifts the rest down by one.
      if (seen.has(key.id)) continue
      seen.add(key.id)
      keys.push(key)
    }
    if (list.items.length < INGEST_KEYS_PAGE_SIZE || keys.length >= total) break
  }
  return { keys, total, unread: Math.max(0, total - keys.length) }
}

export interface IngestKeyState {
  activeKeys: AnalyticsIngestKey[]
  revokedKeys: AnalyticsIngestKey[]
  /** The newest active key: the one the setup snippets use. */
  primaryKey: AnalyticsIngestKey | undefined
}

export function ingestKeyState(keys: AnalyticsIngestKey[]): IngestKeyState {
  const activeKeys = keys.filter((k) => k.is_active)
  return {
    activeKeys,
    revokedKeys: keys.filter((k) => !k.is_active),
    primaryKey: activeKeys[0],
  }
}

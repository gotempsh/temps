// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Frecency: frequency × recency scoring for the command palette.
 *
 * Mozilla-inspired algorithm using exponential decay:
 *   score = useCount * 0.5 ^ (ageDays / HALF_LIFE_DAYS)
 *
 * Stored per-device in localStorage. No network, no PII, no server schema.
 */

const STORAGE_KEY = 'temps:frecency:v1'
const HALF_LIFE_MS = 14 * 24 * 60 * 60 * 1000 // 14 days
const MAX_ENTRIES = 200 // LRU cap so the store stays small

export interface FrecencyEntry {
  count: number
  lastUsedMs: number
}

export type FrecencyStore = Record<string, FrecencyEntry>

export function loadStore(): FrecencyStore {
  if (typeof window === 'undefined') return {}
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' ? (parsed as FrecencyStore) : {}
  } catch {
    return {}
  }
}

export function saveStore(store: FrecencyStore): void {
  if (typeof window === 'undefined') return
  try {
    const entries = Object.entries(store)
    // LRU evict if oversized: drop oldest by lastUsedMs
    let trimmed: FrecencyStore = store
    if (entries.length > MAX_ENTRIES) {
      const sorted = entries.sort(
        (a, b) => b[1].lastUsedMs - a[1].lastUsedMs
      )
      trimmed = Object.fromEntries(sorted.slice(0, MAX_ENTRIES))
    }
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed))
  } catch {
    // localStorage may be full or disabled; silently ignore
  }
}

export function recordUsage(store: FrecencyStore, key: string): FrecencyStore {
  const existing = store[key]
  const next: FrecencyStore = {
    ...store,
    [key]: {
      count: (existing?.count ?? 0) + 1,
      lastUsedMs: Date.now(),
    },
  }
  saveStore(next)
  return next
}

export function frecencyScore(
  entry: FrecencyEntry | undefined,
  now = Date.now()
): number {
  if (!entry) return 0
  const age = Math.max(0, now - entry.lastUsedMs)
  const decay = Math.pow(0.5, age / HALF_LIFE_MS)
  return entry.count * decay
}

/**
 * Normalize raw frecency to 0..~1 with diminishing returns.
 * 5 uses today → 0.5; 20 uses today → 0.8; etc.
 */
export function normalizeFrecency(raw: number): number {
  if (raw <= 0) return 0
  return raw / (raw + 5)
}

/**
 * Combine Fuse text relevance with frecency.
 * relevance ∈ [0,1], 1 = perfect text match.
 * Returns blended score where higher is better.
 */
export function combinedScore(
  relevance: number,
  freqRaw: number,
  weights: { relevance: number; frecency: number } = {
    relevance: 0.7,
    frecency: 0.3,
  }
): number {
  return relevance * weights.relevance + normalizeFrecency(freqRaw) * weights.frecency
}

/**
 * Top N keys ranked by raw frecency. Used for the empty-query "Recent" group.
 */
export function topRecent(store: FrecencyStore, limit = 7): string[] {
  const now = Date.now()
  return Object.entries(store)
    .map(([key, entry]) => [key, frecencyScore(entry, now)] as const)
    .filter(([, score]) => score > 0)
    .sort((a, b) => b[1] - a[1])
    .slice(0, limit)
    .map(([key]) => key)
}

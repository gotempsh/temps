// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState } from 'react'
import {
  combinedScore,
  frecencyScore,
  loadStore,
  recordUsage,
  topRecent,
  type FrecencyStore,
} from '@/lib/frecency'

/**
 * React wrapper around the localStorage-backed frecency store.
 * Re-renders consumers after each `record` so re-rankings update live.
 */
export function useFrecency() {
  const [store, setStore] = useState<FrecencyStore>(() => loadStore())

  const record = useCallback((key: string) => {
    setStore((prev) => recordUsage(prev, key))
  }, [])

  const getScore = useCallback(
    (key: string) => frecencyScore(store[key]),
    [store]
  )

  const blend = useCallback(
    (key: string, relevance: number) =>
      combinedScore(relevance, frecencyScore(store[key])),
    [store]
  )

  const recent = useCallback((limit = 7) => topRecent(store, limit), [store])

  return { record, getScore, blend, recent, store }
}

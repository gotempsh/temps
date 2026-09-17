// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useMemo } from 'react'
import { useSearchParams } from 'react-router'

export type UrlStateValue = string | number | boolean | undefined | null

/**
 * Generalizes the URL-state pattern already used by `useGlobalView`
 * (web/src/hooks/useGlobalView.ts) — read query params as a typed object,
 * patch them immutably, never lose params you didn't touch. Where
 * `useGlobalView` is specific to the observability time-window filters, this
 * is the general-purpose version: any list, detail, or settings page whose
 * filters/tab/page number should survive a refresh or a shared link uses
 * this instead of local `useState`. See RULES.md: "the URL is the state".
 *
 * `undefined`/`null` in a patch deletes the key; everything else is
 * stringified. Reads are always strings — parse at the call site (the
 * `fmt.ts` helpers or a small local `Number(...)`/enum guard), keeping this
 * hook free of per-page parsing logic.
 */
export function useUrlState<K extends string = string>() {
  const [params, setParams] = useSearchParams()

  const get = useCallback((key: K) => params.get(key), [params])

  const getAll = useCallback((key: K) => params.getAll(key), [params])

  const state = useMemo(() => {
    const out: Record<string, string> = {}
    for (const [key, value] of params.entries()) out[key] = value
    return out as Record<K, string | undefined>
  }, [params])

  const patch = useCallback(
    (values: Partial<Record<K, UrlStateValue>>, options?: { replace?: boolean }) => {
      setParams(
        (current) => {
          const next = new URLSearchParams(current)
          for (const [key, value] of Object.entries(values)) {
            if (value === undefined || value === null || value === '') {
              next.delete(key)
            } else {
              next.set(key, String(value))
            }
          }
          return next
        },
        { replace: options?.replace ?? true },
      )
    },
    [setParams],
  )

  const clear = useCallback(
    (keys: K[], options?: { replace?: boolean }) => {
      setParams(
        (current) => {
          const next = new URLSearchParams(current)
          for (const key of keys) next.delete(key)
          return next
        },
        { replace: options?.replace ?? true },
      )
    },
    [setParams],
  )

  return { params, state, get, getAll, patch, clear }
}

export type UrlState = ReturnType<typeof useUrlState>

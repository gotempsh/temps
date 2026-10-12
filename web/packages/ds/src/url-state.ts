// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef } from 'react'
import { useSearchParams, type NavigateOptions } from 'react-router'

export type UrlStateValue = string | number | boolean | undefined | null

/** How a caller describes the next query string: a value, or a function of the latest one. */
export type SearchParamsUpdate =
  | URLSearchParams
  | ((current: URLSearchParams) => URLSearchParams)

/**
 * Chains query-string updates that are issued before the router has rendered
 * the previous one.
 *
 * React Router's `setSearchParams(fn)` hands `fn` the params of the render the
 * callback was created in, not the location the previous call just asked for.
 * Two updates in one tick (`q` and then `page`, or a canonicalisation effect
 * landing next to a keystroke) therefore both start from the same base and the
 * later one silently overwrites the earlier. This buffer remembers the result
 * of the last update, and starts the next one from it for as long as the router
 * has not yet rendered a different location.
 */
export interface SearchParamsBatch {
  /** Resolve `update` against the latest known params and remember the result. */
  apply(rendered: URLSearchParams, update: SearchParamsUpdate): URLSearchParams
  /** Report the location the router has actually rendered. */
  observe(rendered: URLSearchParams): void
}

interface PendingSearch {
  /** The rendered query string the chain started from. */
  from: string
  /** The query string after each update in the chain, in order. */
  steps: string[]
  next: URLSearchParams
  at: number
}

const MAX_CHAIN_LENGTH = 32

export function createSearchParamsBatch(
  options: { maxAgeMs?: number; now?: () => number } = {},
): SearchParamsBatch {
  const maxAgeMs = options.maxAgeMs ?? 2000
  const now = options.now ?? Date.now
  let pending: PendingSearch | null = null

  const continuesFrom = (key: string) =>
    pending !== null &&
    now() - pending.at <= maxAgeMs &&
    (pending.from === key || pending.steps.includes(key))

  return {
    apply(rendered, update) {
      const key = rendered.toString()
      const chained = continuesFrom(key) ? pending : null
      const base = chained ? chained.next : rendered
      const next =
        typeof update === 'function'
          ? update(new URLSearchParams(base))
          : new URLSearchParams(update)
      const steps = chained ? chained.steps : []
      pending = {
        from: chained ? chained.from : key,
        steps: [...steps, next.toString()].slice(-MAX_CHAIN_LENGTH),
        next: new URLSearchParams(next),
        at: now(),
      }
      return next
    },
    observe(rendered) {
      if (pending === null) return
      const key = rendered.toString()
      if (pending.from === key) return
      const index = pending.steps.indexOf(key)
      // An intermediate step of our own chain is not the end of it; anything
      // else (the final step landing, or an external navigation) is.
      if (index >= 0 && index < pending.steps.length - 1) return
      pending = null
    },
  }
}

// One browser location means one chain, shared by every component's hook.
const sharedBatch = createSearchParamsBatch()

/**
 * Drop-in replacement for React Router's `useSearchParams` whose setter is
 * safe to call several times in one tick and from stale closures: each update
 * is applied on top of the previous one instead of on top of the render-time
 * params. The setter's identity is stable.
 */
export function useBatchedSearchParams() {
  const [params, setParams] = useSearchParams()
  const renderedRef = useRef(params)
  const setParamsRef = useRef(setParams)
  // Layout effects run before any passive effect of the same commit, so
  // every effect and event handler sees the location this render is for.
  useLayoutEffect(() => {
    renderedRef.current = params
    setParamsRef.current = setParams
  })

  useEffect(() => {
    sharedBatch.observe(params)
  }, [params])

  const setBatchedParams = useCallback(
    (update: SearchParamsUpdate, options?: NavigateOptions) => {
      const next = sharedBatch.apply(renderedRef.current, update)
      setParamsRef.current(next, options)
    },
    [],
  )

  return [params, setBatchedParams] as const
}

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
  const [params, setParams] = useBatchedSearchParams()

  const get = useCallback((key: K) => params.get(key), [params])

  const getAll = useCallback((key: K) => params.getAll(key), [params])

  const state = useMemo(() => {
    const out: Record<string, string> = {}
    for (const [key, value] of params.entries()) out[key] = value
    return out as Record<K, string | undefined>
  }, [params])

  const patch = useCallback(
    (values: Partial<Record<K, UrlStateValue>>, options?: { replace?: boolean }) => {
      // One update for every key, so a search term and its page reset can
      // never overwrite each other.
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

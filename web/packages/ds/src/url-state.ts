// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import {
  UNSAFE_NavigationContext,
  useLocation,
  useSearchParams,
} from 'react-router'

export type UrlStateValue = string | number | boolean | undefined | null

/**
 * The most recent update each router has not rendered yet, shared by every
 * `useComposedSearchParams` instance under that router. Instances must share
 * it: a search box, a tab strip and a date picker on one page each own a hook,
 * and an edit in one followed by a click in another before the router commits
 * must build on the first, not on the stale rendered params. It is keyed by the
 * router's navigator (so independent routers never see each other's writes),
 * only honoured for the location (`key`) and rendered query (`base`) it was
 * created against, and dropped whenever an instance renders a new location or
 * query.
 */
const pendingByRouter = new WeakMap<
  object,
  { key: string; base: string; next: URLSearchParams }
>()

/**
 * `useSearchParams` whose updater composes across calls.
 *
 * React Router hands a functional updater the params from the *render* that
 * created `setParams`, not the latest navigation. Two updates in one event
 * (`q` then `page`, or an event handler plus a canonicalising effect) would
 * both start from the same stale params and the second would overwrite the
 * first. `pendingByRouter` holds the result of the most recent update until the
 * router renders it, so consecutive updates -- from this instance or any other
 * on the page -- build on each other instead.
 */
export function useComposedSearchParams() {
  const [params, setParams] = useSearchParams()
  const { key } = useLocation()
  const { navigator } = useContext(UNSAFE_NavigationContext)
  const keyRef = useRef(key)
  // Cleared on any new location, not only a new query: a navigation that
  // supersedes our update and lands on the query already rendered keeps the
  // same `params` identity, and the cancelled snapshot would otherwise be
  // resurrected by the next update.
  useEffect(() => {
    keyRef.current = key
    pendingByRouter.delete(navigator)
  }, [params, key, navigator])

  const update = useCallback(
    (
      transform: (current: URLSearchParams) => URLSearchParams,
      options?: { replace?: boolean }
    ) => {
      setParams(
        (current) => {
          const base = current.toString()
          const pending = pendingByRouter.get(navigator)
          const start =
            pending && pending.key === keyRef.current && pending.base === base
              ? pending.next
              : current
          const next = transform(new URLSearchParams(start))
          pendingByRouter.set(navigator, {
            key: keyRef.current,
            base,
            next,
          })
          return next
        },
        { replace: options?.replace ?? true }
      )
    },
    [setParams, navigator]
  )

  return [params, update] as const
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
  const [params, update] = useComposedSearchParams()

  const get = useCallback((key: K) => params.get(key), [params])

  const getAll = useCallback((key: K) => params.getAll(key), [params])

  const state = useMemo(() => {
    const out: Record<string, string> = {}
    for (const [key, value] of params.entries()) out[key] = value
    return out as Record<K, string | undefined>
  }, [params])

  // All keys in `values` land in one navigation, so a search term and its page
  // reset are patched together rather than as two racing updates.
  const patch = useCallback(
    (
      values: Partial<Record<K, UrlStateValue>>,
      options?: { replace?: boolean }
    ) => {
      update((next) => {
        for (const [key, value] of Object.entries(values)) {
          if (value === undefined || value === null || value === '') {
            next.delete(key)
          } else {
            next.set(key, String(value))
          }
        }
        return next
      }, options)
    },
    [update]
  )

  const clear = useCallback(
    (keys: K[], options?: { replace?: boolean }) => {
      update((next) => {
        for (const key of keys) next.delete(key)
        return next
      }, options)
    },
    [update]
  )

  return { params, state, get, getAll, patch, clear }
}

export type UrlState = ReturnType<typeof useUrlState>

/**
 * A text box whose value lives in the URL, without dropping keystrokes.
 *
 * Reading the box straight from the URL is not safe: the router applies
 * navigations in a transition, so a controlled input is briefly reset to the
 * previous URL value and a character typed in that window is lost. This keeps
 * the text in local state (so the box always shows exactly what was typed) and
 * mirrors every change to the URL through `commit`.
 *
 * When the URL changes for any reason other than our own writes (back/forward,
 * a "clear filters" link) the new value is adopted. Writes still in flight are
 * remembered so a late echo of an older keystroke never overwrites newer text.
 */
export function useUrlBoundText(
  urlValue: string,
  commit: (value: string) => void,
  { settleMs = 1000 }: { settleMs?: number } = {}
): [string, (value: string) => void] {
  const [text, setText] = useState(urlValue)
  const { key } = useLocation()
  const inFlight = useRef<string[]>([])
  const previousUrl = useRef(urlValue)
  const latest = useRef({ text, urlValue, lastWriteAt: 0 })
  useEffect(() => {
    latest.current.text = text
    latest.current.urlValue = urlValue
  })

  useEffect(() => {
    if (previousUrl.current !== urlValue) {
      previousUrl.current = urlValue
      const index = inFlight.current.lastIndexOf(urlValue)
      if (index < 0) {
        inFlight.current = []
        setText(urlValue)
        return
      }
      inFlight.current = inFlight.current.slice(index + 1)
    }

    // A write can be cancelled by a navigation that wins the race (Back, a
    // "clear filters" action): the URL then never shows it, and it would stay
    // "in flight" forever while the box keeps the text. Once the URL has been
    // quiet for `settleMs` since the last keystroke, the URL wins.
    let timer: ReturnType<typeof setTimeout> | undefined
    const settle = () => {
      const { text, urlValue, lastWriteAt } = latest.current
      if (text === urlValue) {
        inFlight.current = []
        return
      }
      const quiet = Date.now() - lastWriteAt
      if (quiet < settleMs) {
        timer = setTimeout(settle, settleMs - quiet)
        return
      }
      inFlight.current = []
      setText(urlValue)
    }
    if (inFlight.current.length > 0 || latest.current.text !== urlValue) {
      timer = setTimeout(settle, settleMs)
    }
    return () => clearTimeout(timer)
  }, [urlValue, key, settleMs])

  const update = useCallback(
    (value: string) => {
      setText(value)
      latest.current.lastWriteAt = Date.now()
      inFlight.current.push(value)
      commit(value)
    },
    [commit]
  )

  return [text, update]
}

/**
 * The part of a `useUrlState<K>()` result that `useUrlSearchText` needs. Typed
 * with `never` parameters so any page's key union is accepted.
 */
export type UrlStateHandle = {
  get: (key: never) => string | null
  patch: (values: never, options?: { replace?: boolean }) => void
}

/**
 * `useUrlBoundText` for the common case: one search box backed by one query
 * parameter (`q` by default). Typed text stays local, the URL is written with
 * `replace`, and `resetKeys` (usually `['page']`) are cleared in the same
 * navigation so a new term never leaves the list on a page that no longer
 * exists. Returns `[text, setText, committed]`; `committed` is the value
 * currently in the URL, for callers that need to key a request on it.
 *
 * Use this for every list/search box rather than `useState`, so reload,
 * back/forward and a shared link keep the filter.
 */
export function useUrlSearchText(
  key = 'q',
  {
    resetKeys = [],
    settleMs,
    urlState,
  }: {
    resetKeys?: readonly string[]
    settleMs?: number
    /**
     * The page's own `useUrlState()` when other controls patch the same query
     * string. Updates compose through one pending snapshot per hook instance,
     * so a search edit and a tab/filter click in the same tick only both land
     * when they go through the same instance.
     */
    urlState?: UrlStateHandle
  } = {}
): [string, (value: string) => void, string] {
  const own = useUrlState<string>()
  const { get, patch } = urlState ?? own
  const committed = get(key as never) ?? ''
  const resets = resetKeys.join('\u0000')
  const commit = useCallback(
    (value: string) => {
      const values: Record<string, UrlStateValue> = {
        [key]: value || undefined,
      }
      for (const reset of resets ? resets.split('\u0000') : []) {
        values[reset] = undefined
      }
      patch(values as never)
    },
    [key, resets, patch]
  )
  const [text, setText] = useUrlBoundText(committed, commit, { settleMs })
  return [text, setText, committed]
}

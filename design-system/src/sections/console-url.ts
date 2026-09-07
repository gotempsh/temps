// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router'

/* ────────────────────────────────────────────────────────────────────────
   The URL is the state (`docs/requirements.md`).

   Every screen in the console keeps the view the reader is looking at in the
   query string: the facet, the filter, the sort, the page, the range. A
   reload, a pasted link and a second tab all rebuild the same screen because
   they carry the same address, and nothing that changes what is on screen
   lives only in `useState`.

   The rules this file enforces so no screen has to remember them:

   - Typed keys. `ViewKey` is the whole vocabulary; a screen cannot invent
     `?tabb=` and quietly stop being linkable.
   - Defaults are omitted. Writing the fallback deletes the key, so the common
     address has no query at all and a URL reads as the deltas from default.
   - Unknown values fall back. A hand-edited `?tab=nope` renders the default
     facet rather than an empty page.
   - Replace for a view change, push for a navigation. Typing a filter must
     not put one history entry on the stack per keystroke.
   - Every write goes through `setParams`'s functional form. Two keystrokes can
     land before React re-renders, and a setter that closed over the params of
     its own render would compute the second from the state before the first —
     which is how a filter box silently drops characters.
   ──────────────────────────────────────────────────────────────────────── */

/**
 * The address as the browser has it at this instant, which is not always what
 * the last render saw: react-router renders navigations in a transition, so two
 * keystrokes can land before the hook's `params` catches up. Every write starts
 * from here, so a filter box cannot drop letters.
 */
const live = () => new URLSearchParams(window.location.search)


/**
 * The query keys a screen may write. `p` is the route (which record) and is
 * owned by `ConsoleV1Page`; `fresh` and `fail` are sandbox demo flags that
 * outlive a navigation. Everything else here is view state: it belongs to the
 * screen currently on the page and is dropped when the reader leaves it.
 */
export const VIEW_KEYS = [
  'tab', // the facet / tab of a record or tool
  'f', // filter text (the `/` filter of a Ledger)
  'sort', // sort key
  'page', // 1-based page of a paged ledger
  'range', // time window: 1h · 24h · 7d · 30d
  'sel', // a window brushed on a chart, written `from~to`
  'row', // the row read in an Inspector beside the list
  'resrange', // the resources section's own window, on a record that already owns `range`
  'env', // the scope Picker: which environment a list is about
  'seg', // a Segmented: which rendering of the same list
  'dim', // which dimension a breakdown or table is cut by
  'metric', // which measure is plotted or ranked on
  'device', // desktop · mobile, where a screen splits by client
  'step', // which step of a funnel or wizard
  'size', // page size of a paged ledger
  'q', // the Logs query (tokens + text), shared grammar
  'cols', // the Logs trailing columns
  'lv', // the Logs rendering: list · grouped · owners
] as const

export type ViewKey = (typeof VIEW_KEYS)[number]

/** Keys that survive a navigation between records: the route and the sandbox's demo flags. */
const KEPT_ON_NAVIGATION = new Set(['p', 'fresh', 'fail'])

/**
 * Drop the view state when the reader goes to another record. Without this a
 * `?tab=deploys` from a project record would follow them onto the settings
 * page and select whatever its third facet happens to be — the same address
 * meaning two different things, which is the bug this whole file exists to
 * prevent.
 */
export function forNewView(params: URLSearchParams): URLSearchParams {
  const next = new URLSearchParams()
  for (const [k, v] of params) if (KEPT_ON_NAVIGATION.has(k)) next.append(k, v)
  return next
}

/* `NoInfer` on the fallback so `useUrlState('f', '')` is a string filter and not
   the literal type `''`: the value is state the reader types into, and inferring
   the default as the whole type would make the setter reject every other word. */
type Options<T extends string> = {
  /** The values this key accepts. Anything else in the URL reads as the fallback. */
  values?: readonly T[]
  /** Push a history entry instead of replacing. Navigation pushes; looking at the same thing differently replaces. */
  push?: boolean
}

/**
 * One query key as a piece of state. Reads like `useState` and writes like a
 * link: the value is always what the address says, and setting it back to the
 * fallback removes the key.
 */
export function useUrlState<T extends string = string>(key: ViewKey, fallback: NoInfer<T>, options: Options<T> = {}): [T, (next: T) => void] {
  const [params, setParams] = useSearchParams()
  const { values, push = false } = options
  const raw = params.get(key)
  const value = useMemo<T>(() => {
    if (raw === null) return fallback
    if (values && !values.includes(raw as T)) return fallback
    return raw as T
  }, [raw, fallback, values])
  const set = useCallback(
    (next: T) => {
      setParams(() => {
        const p = live()
        if (next === fallback || next === '') p.delete(key)
        else p.set(key, next)
        return p
      }, { replace: !push })
    },
    [setParams, key, fallback, push],
  )
  return [value, set]
}

/** The same, for a key whose value is a number: the page of a ledger. */
export function useUrlNumber(key: ViewKey, fallback: number, options: { push?: boolean } = {}): [number, (next: number) => void] {
  const [params, setParams] = useSearchParams()
  const { push = false } = options
  const raw = params.get(key)
  const value = useMemo(() => {
    const n = Number(raw)
    return raw !== null && Number.isFinite(n) && n >= 1 ? Math.floor(n) : fallback
  }, [raw, fallback])
  const set = useCallback(
    (next: number) => {
      setParams(() => {
        const p = live()
        if (next === fallback) p.delete(key)
        else p.set(key, String(next))
        return p
      }, { replace: !push })
    },
    [setParams, key, fallback, push],
  )
  return [value, set]
}

/**
 * Several keys at once, as one history entry. A filter that also resets the
 * page is one change to the view, not two — writing them separately would put
 * the reader on page 4 of the new filter for a frame, and would give `back`
 * two steps to undo one action.
 */
export function useUrlPatch(): (next: Partial<Record<ViewKey, string | null>>, options?: { push?: boolean }) => void {
  const [, setParams] = useSearchParams()
  return useCallback(
    (next, options = {}) => {
      setParams(() => {
        const p = live()
        for (const [k, v] of Object.entries(next)) {
          if (v === null || v === '') p.delete(k)
          else p.set(k, v)
        }
        return p
      }, { replace: !options.push })
    },
    [setParams],
  )
}

/** A window brushed on a chart. Written `from~to` so it reads in the address bar. */
export type UrlWindow = { from: string; to: string }

/**
 * A chart selection as a query key. A brushed window narrows the list under
 * the chart, so it is view state like any other: linkable, and back where it
 * was after a reload.
 */
export function useUrlWindow(key: ViewKey = 'sel'): [UrlWindow | null, (next: UrlWindow | null) => void] {
  const [params, setParams] = useSearchParams()
  const raw = params.get(key)
  const value = useMemo<UrlWindow | null>(() => {
    if (!raw) return null
    const [from, to] = raw.split('~')
    return from && to ? { from, to } : null
  }, [raw])
  const set = useCallback(
    (next: UrlWindow | null) => {
      setParams(() => {
        const p = live()
        if (!next) p.delete(key)
        else p.set(key, `${next.from}~${next.to}`)
        return p
      }, { replace: true })
    },
    [setParams, key],
  )
  return [value, set]
}

/** A ledger's sort, written `?sort=key` ascending and `?sort=-key` descending. */
export type UrlSort = { key: string; dir: 'asc' | 'desc' } | null

/**
 * `Ledger` takes a controlled `sort` / `onSort` pair, so the column the reader
 * chose is view state like any other and does not have to stay inside the
 * primitive. Only wire it on a ledger that sorts its own rows: a paged ledger
 * holds one page, and sorting that page would reorder 20 of N.
 */
export function useUrlSort(key: ViewKey = 'sort'): [UrlSort, (next: UrlSort) => void] {
  const [params, setParams] = useSearchParams()
  const raw = params.get(key)
  const value = useMemo<UrlSort>(() => {
    if (!raw) return null
    return raw.startsWith('-') ? { key: raw.slice(1), dir: 'desc' } : { key: raw, dir: 'asc' }
  }, [raw])
  const set = useCallback(
    (next: UrlSort) => {
      setParams(() => {
        const p = live()
        if (!next) p.delete(key)
        else p.set(key, next.dir === 'desc' ? `-${next.key}` : next.key)
        return p
      }, { replace: true })
    },
    [setParams, key],
  )
  return [value, set]
}

/**
 * Filter text, in the URL and in the box at the same time.
 *
 * A text field needs a value on the keystroke; a navigation lands a frame or
 * two later, and typing faster than that would make the box drop letters. So
 * the field keeps a draft, every change writes the address, and the address
 * wins whenever it changes from outside — a reload, a pasted link, `back`, or
 * the "clear" button under an empty ledger. There is still one truth: the
 * draft is only ever a repaint of what was just written.
 */
export function useUrlText(key: ViewKey = 'f', also?: Partial<Record<ViewKey, string | null>>): [string, (next: string) => void] {
  const [params] = useSearchParams()
  const patch = useUrlPatch()
  const fromUrl = params.get(key) ?? ''
  const [draft, setDraft] = useState(fromUrl)
  /* What this field last wrote, until the address catches up. While a write is
     in flight the URL walks through the letters already typed, and following it
     would rewind the box; once it arrives, the field is settled again and the
     address is the only thing it reads. */
  const written = useRef<string | null>(null)
  if (written.current === null) {
    // Settled: the address moved and it was not this field that moved it.
    if (fromUrl !== draft) setDraft(fromUrl)
  } else if (fromUrl === written.current) {
    written.current = null
  }
  const set = useCallback(
    (next: string) => {
      written.current = next
      setDraft(next)
      patch({ ...also, [key]: next || null })
    },
    // `also` is written inline at the call site; comparing it by identity would
    // rebuild the setter every render for no gain, so it is read as it is.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [patch, key],
  )
  return [draft, set]
}

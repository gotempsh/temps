// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLayoutEffect, useRef, useState, type RefObject } from 'react'
import { useLocation, useNavigate } from 'react-router'

/** One snapshot per route owner, discarded when that resource's routes unmount. */
export interface ListReturnState {
  url: string
  timeRange: string
  dateRange: { from: string; to: string }
  scrollTop: number
  scrollLeft: number
}

export function useListReturnNavigation(basePath: string) {
  const [listState, setListState] = useState<ListReturnState | null>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const { pathname } = useLocation()
  useLayoutEffect(() => {
    if (pathname.replace(/\/$/, '') !== basePath) {
      const scroller = containerRef.current?.closest<HTMLElement>(
        '[data-console-scroll]'
      )
      if (scroller) scroller.scrollTop = 0
    }
  }, [pathname, basePath])
  return { listState, setListState, containerRef }
}

export function useListScrollRestoration(
  containerRef: RefObject<HTMLDivElement | null>,
  saved: ListReturnState | null | undefined,
  ready: boolean,
  url: string
) {
  const pendingRef = useRef(saved)
  useLayoutEffect(() => {
    const pending = pendingRef.current
    if (!pending) return
    if (pending.url !== url) {
      pendingRef.current = null
      return
    }
    if (!ready) return
    const scroller = containerRef.current?.closest<HTMLElement>(
      '[data-console-scroll]'
    )
    if (scroller) scroller.scrollTop = pending.scrollTop
    const tableScroller =
      containerRef.current?.querySelector('table')?.parentElement
    if (tableScroller) tableScroller.scrollLeft = pending.scrollLeft
    pendingRef.current = null
  }, [containerRef, ready, url])
}

export function captureListReturnState(
  container: HTMLElement | null,
  view: Pick<ListReturnState, 'url' | 'timeRange' | 'dateRange'>
): ListReturnState {
  return {
    ...view,
    scrollTop:
      container?.closest<HTMLElement>('[data-console-scroll]')?.scrollTop ?? 0,
    scrollLeft:
      container?.querySelector('table')?.parentElement?.scrollLeft ?? 0,
  }
}

export function useListBack(basePath: string, saved?: ListReturnState | null) {
  const navigate = useNavigate()
  const location = useLocation()
  return () => {
    const returnTo =
      saved && saved.url.split('?')[0].replace(/\/$/, '') === basePath
        ? saved.url
        : basePath
    if (location.state?.listReturnTo === returnTo) navigate(-1)
    else navigate(returnTo)
  }
}

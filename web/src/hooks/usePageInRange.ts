// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'

/** One loaded page of a paginated list, counted from 1. */
export interface LoadedPage {
  /** Rows the page came back with. */
  rows: number
  /** Pages the whole list has now. */
  totalPages: number
}

/**
 * The page to show for `page` given what it loaded: itself, unless it came
 * back empty past the end of the list, then the list's last page (1 when
 * the list is empty). Refresh keeps the page while it moves a window to now;
 * if the moved window holds fewer pages, this is the way back to one that
 * exists, instead of a false empty state with no pagination to leave it.
 */
export function pageInRange(page: number, loaded: LoadedPage): number {
  if (page <= 1 || loaded.rows > 0 || page <= loaded.totalPages) return page
  return Math.max(1, loaded.totalPages)
}

/**
 * Go to the last page that exists when `page` loaded empty past the end of
 * the list (see `pageInRange`). Pass `loaded` only for an answer to this very
 * page: not while another page's or window's rows stand in for it.
 */
export function usePageInRange(
  page: number,
  loaded: LoadedPage | undefined,
  setPage: (page: number) => void
): void {
  const target = loaded ? pageInRange(page, loaded) : page
  useEffect(() => {
    if (target !== page) setPage(target)
  }, [target, page, setPage])
}

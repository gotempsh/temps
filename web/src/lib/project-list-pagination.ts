// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const DEFAULT_PROJECT_PAGE = 1
export const DEFAULT_PROJECT_PAGE_SIZE = 9
export const PROJECT_PAGE_SIZE_OPTIONS = [9, 18, 36, 72] as const

export interface ProjectPagination {
  page: number
  pageSize: number
}

function parsePositiveInteger(value: string | null): number | undefined {
  if (!value || !/^\d+$/.test(value)) return undefined

  const parsed = Number(value)
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined
}

export function readProjectPagination(
  searchParams: URLSearchParams
): ProjectPagination {
  const page =
    parsePositiveInteger(searchParams.get('page')) ?? DEFAULT_PROJECT_PAGE
  const requestedPageSize = parsePositiveInteger(searchParams.get('page_size'))
  const pageSize =
    requestedPageSize !== undefined &&
    (PROJECT_PAGE_SIZE_OPTIONS as readonly number[]).includes(requestedPageSize)
      ? requestedPageSize
      : DEFAULT_PROJECT_PAGE_SIZE

  return { page, pageSize }
}

export function projectPageCount(total: number, pageSize: number): number {
  if (
    !Number.isFinite(total) ||
    !Number.isFinite(pageSize) ||
    total <= 0 ||
    pageSize <= 0
  ) {
    return 1
  }
  return Math.max(1, Math.ceil(total / pageSize))
}

/**
 * Whether the list needs pagination controls at all. A list that fits on the
 * first page at the smallest page size has nothing to page through or resize,
 * and "Page 1 of 1" with a page-size picker under one card is noise.
 */
export function showsProjectPagination(total: number, page: number): boolean {
  return page > 1 || total > Math.min(...PROJECT_PAGE_SIZE_OPTIONS)
}

export function withProjectPagination(
  current: URLSearchParams,
  pagination: ProjectPagination
): URLSearchParams {
  const next = new URLSearchParams(current)
  next.set('page', String(pagination.page))
  next.set('page_size', String(pagination.pageSize))
  return next
}

/** A filter starts at page one and preserves the rest of the current view. */
export function withProjectSearch(
  current: URLSearchParams,
  value: string
): URLSearchParams {
  const next = new URLSearchParams(current)
  if (value) next.set('q', value)
  else next.delete('q')
  next.set('page', '1')
  return next
}

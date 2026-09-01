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

export function withProjectPagination(
  current: URLSearchParams,
  pagination: ProjectPagination
): URLSearchParams {
  const next = new URLSearchParams(current)
  next.set('page', String(pagination.page))
  next.set('page_size', String(pagination.pageSize))
  return next
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `--page` / `--page-size` for list commands whose endpoint answers with a
 * page envelope (`{ items, total, page, page_size }`).
 */

/** The query a paginated list endpoint takes. */
export interface PageQuery {
  page?: number
  page_size?: number
}

/** The flags as commander hands them over. */
export interface PageFlags {
  page?: string
  pageSize?: string
}

/** The most rows one page may hold, as the API enforces it. */
export const MAX_PAGE_SIZE = 100

function positiveInteger(flag: string, value: string): number {
  if (!/^\d+$/.test(value) || Number(value) <= 0) {
    throw new Error(`Invalid ${flag} '${value}': expected a positive integer`)
  }
  return Number(value)
}

/**
 * The page query for the given flags. An omitted flag is left out, so the
 * server applies its default (page 1, 20 rows).
 */
export function pageQuery(flags: PageFlags): PageQuery {
  const query: PageQuery = {}
  if (flags.page !== undefined) query.page = positiveInteger('--page', flags.page)
  if (flags.pageSize !== undefined) {
    const size = positiveInteger('--page-size', flags.pageSize)
    if (size > MAX_PAGE_SIZE) {
      throw new Error(
        `Invalid --page-size '${flags.pageSize}': at most ${MAX_PAGE_SIZE}`,
      )
    }
    query.page_size = size
  }
  return query
}

/** One page of a list as the API returns it. */
export interface PageEnvelope {
  total: number
  page: number
  page_size: number
}

/**
 * The line that tells the reader there is more, or `undefined` on the last
 * page.
 */
export function nextPageHint(result: PageEnvelope): string | undefined {
  if (result.total <= result.page * result.page_size) return undefined
  return `Page ${result.page} of ${Math.ceil(result.total / result.page_size)}; more with --page ${result.page + 1}`
}

/** One page as a paginated list endpoint returns it. */
export interface Page<T> extends PageEnvelope {
  items: T[]
}

/**
 * The first item matching `found`, reading the list a full page at a time
 * until it turns up or the list ends. For lookups by a field the endpoint
 * cannot filter on (a secret by key).
 */
export async function findInPages<T>(
  fetchPage: (query: Required<PageQuery>) => Promise<Page<T>>,
  found: (item: T) => boolean,
): Promise<T | undefined> {
  for (let page = 1; ; page += 1) {
    const result = await fetchPage({ page, page_size: MAX_PAGE_SIZE })
    const match = result.items.find(found)
    if (match) return match
    if (result.items.length === 0 || page * result.page_size >= result.total) {
      return undefined
    }
  }
}

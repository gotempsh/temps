// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { findInPages, nextPageHint, pageQuery } from './page-options.js'

describe('pageQuery', () => {
  test('omitted flags leave the server default in place', () => {
    expect(pageQuery({})).toEqual({})
  })

  test('parses both flags', () => {
    expect(pageQuery({ page: '3', pageSize: '50' })).toEqual({
      page: 3,
      page_size: 50,
    })
  })

  test('rejects zero, negatives, fractions and words', () => {
    for (const page of ['0', '-1', '1.5', 'two']) {
      expect(() => pageQuery({ page })).toThrow(`Invalid --page '${page}'`)
    }
  })

  test('rejects a page size above the API maximum', () => {
    expect(() => pageQuery({ pageSize: '101' })).toThrow('at most 100')
    expect(pageQuery({ pageSize: '100' })).toEqual({ page_size: 100 })
  })
})

describe('nextPageHint', () => {
  test('points at the next page while there is one', () => {
    expect(nextPageHint({ total: 45, page: 1, page_size: 20 })).toBe(
      'Page 1 of 3; more with --page 2',
    )
  })

  test('is silent on the last page', () => {
    expect(nextPageHint({ total: 45, page: 3, page_size: 20 })).toBeUndefined()
    expect(nextPageHint({ total: 0, page: 1, page_size: 20 })).toBeUndefined()
  })
})

describe('findInPages', () => {
  const items = Array.from({ length: 230 }, (_, i) => ({ id: i + 1 }))
  const pages: number[] = []
  const fetchPage = async ({ page, page_size }: { page: number; page_size: number }) => {
    pages.push(page)
    return {
      items: items.slice((page - 1) * page_size, page * page_size),
      total: items.length,
      page,
      page_size,
    }
  }

  test('reads pages until the item turns up, and no further', async () => {
    pages.length = 0
    expect(await findInPages(fetchPage, (item) => item.id === 150)).toEqual({ id: 150 })
    expect(pages).toEqual([1, 2])
  })

  test('stops at the end of the list when nothing matches', async () => {
    pages.length = 0
    expect(await findInPages(fetchPage, (item) => item.id === 999)).toBeUndefined()
    expect(pages).toEqual([1, 2, 3])
  })
})

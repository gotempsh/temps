// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  DEFAULT_PROJECT_PAGE_SIZE,
  projectPageCount,
  readProjectPagination,
  withProjectPagination,
  withProjectSearch,
} from './project-list-pagination'

describe('project list pagination', () => {
  test('reads a page and supported page size from the URL', () => {
    expect(
      readProjectPagination(new URLSearchParams('page=4&page_size=36'))
    ).toEqual({ page: 4, pageSize: 36 })
  })

  test('falls back safely for invalid and unsupported values', () => {
    expect(
      readProjectPagination(new URLSearchParams('page=-2&page_size=1000'))
    ).toEqual({ page: 1, pageSize: DEFAULT_PROJECT_PAGE_SIZE })
    expect(
      readProjectPagination(new URLSearchParams('page=2.5&page_size=18x'))
    ).toEqual({ page: 1, pageSize: DEFAULT_PROJECT_PAGE_SIZE })
  })

  test('calculates page bounds', () => {
    expect(projectPageCount(73, 18)).toBe(5)
    expect(projectPageCount(0, 18)).toBe(1)
    expect(projectPageCount(10, Number.NaN)).toBe(1)
  })

  test('updates pagination without dropping unrelated query parameters', () => {
    const params = withProjectPagination(
      new URLSearchParams('view=compact&page=2'),
      { page: 7, pageSize: 36 }
    )

    expect(params.toString()).toBe('view=compact&page=7&page_size=36')
  })
})

describe('project search view', () => {
  test('preserves the filter across reload and resets pagination without losing view options', () => {
    const current = new URLSearchParams('page=4&page_size=18&view=compact')
    const next = withProjectSearch(current, 'billing & API')
    const reloaded = new URLSearchParams(next.toString())
    expect(reloaded.get('q')).toBe('billing & API')
    expect(readProjectPagination(reloaded)).toEqual({ page: 1, pageSize: 18 })
    expect(reloaded.get('view')).toBe('compact')
    expect(current.get('page')).toBe('4')
  })
  test('clearing search removes the filter and keeps the selected page size', () => {
    const next = withProjectSearch(
      new URLSearchParams('q=missing&page=3&page_size=36'),
      ''
    )
    expect(next.has('q')).toBe(false)
    expect(readProjectPagination(next)).toEqual({ page: 1, pageSize: 36 })
  })
})

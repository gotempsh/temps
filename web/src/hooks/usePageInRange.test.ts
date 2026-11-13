// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, renderHook } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { pageInRange, usePageInRange } from './usePageInRange'

installBrowserWindow()
afterEach(cleanup)

test('a page past the end of a shrunken list goes to the last page', () => {
  expect(pageInRange(5, { rows: 0, totalPages: 4 })).toBe(4)
  expect(pageInRange(3, { rows: 0, totalPages: 0 })).toBe(1)
})

test('a page with rows, the first page, or a page in range stays', () => {
  expect(pageInRange(3, { rows: 1, totalPages: 2 })).toBe(3)
  expect(pageInRange(1, { rows: 0, totalPages: 0 })).toBe(1)
  // An empty page the list still reaches is the server's answer, not a gap.
  expect(pageInRange(2, { rows: 0, totalPages: 3 })).toBe(2)
})

test('the hook moves only on a loaded answer for the page', () => {
  const moves: number[] = []
  const setPage = (page: number) => moves.push(page)
  const { rerender } = renderHook(
    ({ loaded }: { loaded?: { rows: number; totalPages: number } }) =>
      usePageInRange(3, loaded, setPage),
    { initialProps: {} }
  )
  expect(moves).toEqual([])
  rerender({ loaded: { rows: 0, totalPages: 2 } })
  expect(moves).toEqual([2])
})

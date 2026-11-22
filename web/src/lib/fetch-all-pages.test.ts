// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { fetchAllPages } from './fetch-all-pages'

const pagesOf = (total: number) => async (page: number, pageSize: number) => {
  const start = (page - 1) * pageSize
  return Array.from(
    { length: Math.max(0, Math.min(pageSize, total - start)) },
    (_, i) => start + i
  )
}

describe('fetchAllPages', () => {
  test('returns everything past the first page', async () => {
    const all = await fetchAllPages(pagesOf(250), 100)
    expect(all).toHaveLength(250)
    expect(all[249]).toBe(249)
  })

  test('stops after a short page', async () => {
    let calls = 0
    const counted = async (page: number, pageSize: number) => {
      calls++
      return pagesOf(30)(page, pageSize)
    }
    expect(await fetchAllPages(counted, 100)).toHaveLength(30)
    expect(calls).toBe(1)
  })

  test('asks again after an exactly full last page', async () => {
    expect(await fetchAllPages(pagesOf(200), 100)).toHaveLength(200)
  })

  test('is bounded by maxPages', async () => {
    const endless = async (_page: number, pageSize: number) =>
      new Array<number>(pageSize).fill(1)
    expect(await fetchAllPages(endless, 10, 3)).toHaveLength(30)
  })
})

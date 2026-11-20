// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { LatestDeploymentMediaResponseItem } from '@/api/client'
import { MEDIA_BATCH_LIMIT, createMediaBatcher } from './service-media-batcher'

const item = (id: number): LatestDeploymentMediaResponseItem => ({
  project_id: id,
  latest_attempt_status: 'completed',
  url: `https://s${id}.example.com`,
})

describe('createMediaBatcher', () => {
  test('lookups in the same tick share one request; missing ids resolve to null', async () => {
    const calls: number[][] = []
    const load = createMediaBatcher(async (ids) => {
      calls.push(ids)
      return { '1': item(1), '2': item(2) }
    })
    const [a, b, again, missing] = await Promise.all([load(1), load(2), load(1), load(3)])
    expect(calls).toEqual([[1, 2, 3]])
    expect(a?.url).toBe('https://s1.example.com')
    expect(again).toEqual(a)
    expect(b?.project_id).toBe(2)
    expect(missing).toBeNull()
  })

  test('splits large batches at the endpoint limit', async () => {
    const sizes: number[] = []
    const load = createMediaBatcher(async (ids) => {
      sizes.push(ids.length)
      return {}
    })
    await Promise.all(Array.from({ length: MEDIA_BATCH_LIMIT + 5 }, (_, i) => load(i + 1)))
    expect(sizes).toEqual([MEDIA_BATCH_LIMIT, 5])
  })

  test('a failed chunk rejects only its own lookups', async () => {
    const load = createMediaBatcher(async () => {
      throw new Error('403')
    })
    await expect(load(1)).rejects.toThrow('403')
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getLatestDeploymentMedia } from '@/api/client/sdk.gen'
import type { LatestDeploymentMediaResponseItem } from '@/api/client'

export type ServiceMedia = LatestDeploymentMediaResponseItem | null

/** The latest-media endpoint accepts at most this many ids per request. */
export const MEDIA_BATCH_LIMIT = 100

type FetchBatch = (ids: number[]) => Promise<Record<string, LatestDeploymentMediaResponseItem>>

interface Waiter {
  resolve: (media: ServiceMedia) => void
  reject: (error: unknown) => void
}

/**
 * Coalesces per-service media lookups made in the same tick into as few
 * batch requests as the endpoint allows, so a list of icons that each ask
 * for their own media still costs one request. A service with no deployment
 * resolves to null.
 */
export function createMediaBatcher(fetchBatch: FetchBatch) {
  let pending = new Map<number, Waiter[]>()
  let scheduled = false

  const flush = async () => {
    const batch = pending
    pending = new Map()
    scheduled = false
    const ids = [...batch.keys()]
    for (let i = 0; i < ids.length; i += MEDIA_BATCH_LIMIT) {
      const chunk = ids.slice(i, i + MEDIA_BATCH_LIMIT)
      try {
        const projects = await fetchBatch(chunk)
        for (const id of chunk) {
          for (const w of batch.get(id) ?? []) w.resolve(projects[String(id)] ?? null)
        }
      } catch (error) {
        for (const id of chunk) {
          for (const w of batch.get(id) ?? []) w.reject(error)
        }
      }
    }
  }

  return (id: number): Promise<ServiceMedia> =>
    new Promise((resolve, reject) => {
      pending.set(id, [...(pending.get(id) ?? []), { resolve, reject }])
      if (!scheduled) {
        scheduled = true
        setTimeout(() => void flush(), 0)
      }
    })
}

export const loadServiceMedia = createMediaBatcher(async (ids) => {
  const { data } = await getLatestDeploymentMedia({
    query: { project_ids: ids.join(',') },
    throwOnError: true,
  })
  return data.projects
})

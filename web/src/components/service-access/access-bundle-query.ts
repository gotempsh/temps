// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { listAccessBundles } from '@/api/client'
export function accessBundleOptions(
  projectId: number,
  vaultId: string,
  bundleId: string
) {
  return {
    queryKey: ['accessBundle', projectId, vaultId, bundleId],
    retry: false,
    refetchOnMount: 'always' as const,
    queryFn: async ({ signal }: { signal: AbortSignal }) => {
      // The API exposes paginated collections. Resolve direct links without
      // retaining the whole collection or assuming the bundle is on page one.
      const boundedSignal = AbortSignal.any([
        signal,
        AbortSignal.timeout(30000),
      ])
      let pages = 1
      for (let page = 1; page <= pages; page++) {
        const { data } = await listAccessBundles({
          path: { project_id: projectId, vault_id: vaultId },
          query: { page, page_size: 100 },
          signal: boundedSignal,
          throwOnError: true,
        })
        if (page === 1) pages = Math.ceil(data.total / 100)
        const found = data.items.find((item) => item.id === bundleId)
        if (found) return found
      }
      throw new Error(
        'This access bundle was not found in the vault. Return to the bundle list or retry.'
      )
    },
  }
}

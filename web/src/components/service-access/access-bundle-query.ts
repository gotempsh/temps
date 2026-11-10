// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { listAccessBundlesGeneral } from '@/api/client'
export function accessBundleOptions(
  vaultId: string,
  bundleId: string,
  sandboxId?: string
) {
  return {
    queryKey: ['accessBundle', vaultId, bundleId, sandboxId],
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
        const { data } = await listAccessBundlesGeneral({
          path: { vault_id: vaultId },
          query: { page, page_size: 100, sandbox_id: sandboxId },
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

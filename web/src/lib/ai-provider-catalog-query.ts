// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listAiProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProviderCatalogDto, ProviderCatalogResponse } from '@/api/client'
import type { QueryClient } from '@tanstack/react-query'

/** Shared query identity for every consumer of the provider catalog. */
export const aiProviderCatalogQueryOptions = listAiProvidersOptions()

/** A successful credential write is authoritative, even if an older read is in flight. */
export async function publishVerifiedProvider(
  queryClient: QueryClient,
  provider: ProviderCatalogDto
) {
  await queryClient.cancelQueries({
    queryKey: aiProviderCatalogQueryOptions.queryKey,
  })
  queryClient.setQueryData<ProviderCatalogResponse>(
    aiProviderCatalogQueryOptions.queryKey,
    (catalog) =>
      catalog
        ? {
            ...catalog,
            providers: catalog.providers.some((item) => item.id === provider.id)
              ? catalog.providers.map((item) =>
                  item.id === provider.id ? provider : item
                )
              : [...catalog.providers, provider],
          }
        : undefined
  )
}

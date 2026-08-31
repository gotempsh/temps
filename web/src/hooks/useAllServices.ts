// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listServices } from '@/api/client/sdk.gen'
import type { ExternalServiceInfo } from '@/api/client/types.gen'
import { useQuery } from '@tanstack/react-query'

const SERVICE_PAGE_SIZE = 100

export async function collectAllServicePages(
  fetchPage: (page: number, pageSize: number) => Promise<ExternalServiceInfo[]>,
  pageSize = SERVICE_PAGE_SIZE
): Promise<ExternalServiceInfo[]> {
  const services: ExternalServiceInfo[] = []
  for (let page = 1; ; page += 1) {
    const pageServices = await fetchPage(page, pageSize)
    services.push(...pageServices)
    if (pageServices.length < pageSize) return services
  }
}

/** Load the complete database inventory used by project-creation selectors. */
export function useAllServices() {
  return useQuery({
    queryKey: ['external-services', 'all-for-project-creation'],
    queryFn: ({ signal }) =>
      collectAllServicePages(async (page, pageSize) => {
        const { data } = await listServices({
          query: { page, page_size: pageSize },
          signal,
          throwOnError: true,
        })
        return data
      }),
  })
}

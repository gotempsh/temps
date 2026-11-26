// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { queryOptions } from '@tanstack/react-query'
import { listServices } from '@/api/client/sdk.gen'
import { listServicesQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { fetchAllPages } from './fetch-all-pages'

/**
 * Every database on the server. The list endpoint pages (20 by default)
 * and cannot search or filter, so every page is read: a database past the
 * first page must still be linkable, and a server is not called healthy
 * from part of its databases. One query, shared by the sidebar's Server
 * row and the pages that link databases.
 */
export const serverDatabasesQuery = () =>
  queryOptions({
    queryKey: [...listServicesQueryKey(), 'all-pages'],
    queryFn: ({ signal }) =>
      fetchAllPages(async (page, pageSize) => {
        const { data } = await listServices({
          query: { page, page_size: pageSize },
          signal,
          throwOnError: true,
        })
        return data ?? []
      }),
  })

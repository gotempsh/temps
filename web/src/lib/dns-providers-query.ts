// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listDnsProviders } from '@/api/client'

/** Shared with the add/detail pages, which invalidate it after a write. */
export const DNS_PROVIDERS_QUERY_KEY = ['dnsProviders'] as const

/**
 * The DNS provider list.
 *
 * `throwOnError` is what lets a refusal reach the page: without it the SDK
 * resolves with `{ error }`, this function returns `undefined`, and
 * react-query invents "data is undefined" -- losing the 403 and the server's
 * reason along the way.
 */
export function dnsProvidersQueryOptions() {
  return {
    queryKey: DNS_PROVIDERS_QUERY_KEY,
    queryFn: async () => {
      const response = await listDnsProviders({ throwOnError: true })
      return response.data
    },
    retry: false,
  }
}

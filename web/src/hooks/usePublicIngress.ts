// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getPublicIngressStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  publicIngressBaseUrl,
  publicIngressCallbackUrlOrNull,
} from '@/lib/publicIngress'

/**
 * Where third-party callbacks are sent (`GET /settings/public-ingress`).
 *
 * `baseUrl` is what every callback URL shown in the console is built from:
 * a webhook URL to paste into a provider, the GitHub App manifest's hook URL,
 * an agent trigger URL. It is the public ingress URL, which is not the
 * address being browsed when the console is restricted.
 *
 * `baseUrl` and `callbackUrl()` are `null` while the status is loading and
 * after a failed read. They never fall back to the browser origin: that is
 * the private console when a separate ingress URL is configured, and a
 * provider registered against it cannot deliver anything. Callers must keep
 * copy and submit controls disabled while it is `null`, and offer `refetch`
 * when `isError` is set — `PublicCallbackUrl` does both.
 *
 * The endpoint only needs `projects:read`, so it works for every role that
 * can wire a callback.
 */
export function usePublicIngress() {
  const query = useQuery({
    ...getPublicIngressStatusOptions(),
    staleTime: 60 * 1000,
    retry: 1,
  })

  return {
    status: query.data,
    baseUrl: publicIngressBaseUrl(query.data),
    callbackUrl: (path: string) =>
      publicIngressCallbackUrlOrNull(query.data, path),
    isPending: query.isPending,
    isError: query.isError,
    isRefetching: query.isRefetching,
    refetch: query.refetch,
  }
}

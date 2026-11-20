// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient, QueryKey } from '@tanstack/react-query'

// Generated SDK query keys are `[{ _id, ...params }]`, so queries are matched
// on `_id` rather than on a string key.
const DOMAIN_DETAIL_QUERY_IDS: ReadonlySet<string> = new Set([
  'getDomainById',
  'getDomainOrder',
  'getHttpChallengeDebug',
  'getPublicIp',
  'listDnsProviders',
  'listRenewalAttempts',
])

/** True for every query the domain detail page shows and must refresh together. */
export function isDomainDetailQuery(queryKey: QueryKey | undefined): boolean {
  const key = queryKey?.[0]
  if (typeof key !== 'object' || key === null || !('_id' in key)) return false
  const id = (key as { _id: unknown })._id
  return typeof id === 'string' && DOMAIN_DETAIL_QUERY_IDS.has(id)
}

/**
 * Re-read everything the domain detail page shows. Run it after a provisioning
 * action settles, success or failure: a failed validation still persists a
 * failed renewal attempt and a `failed` status server-side.
 */
export async function refreshDomainDetail(
  queryClient: QueryClient,
  opts?: { clearOrder?: QueryKey }
): Promise<void> {
  if (opts?.clearOrder) {
    queryClient.removeQueries({ queryKey: opts.clearOrder })
  }
  await queryClient.invalidateQueries({
    predicate: (q) => isDomainDetailQuery(q.queryKey),
  })
}

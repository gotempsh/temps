// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectOptions } from '@/api/client/@tanstack/react-query.gen'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'

/**
 * A chat's project, for its logo. The list and each row share this query (same
 * key) and it never polls; logos are background context, so a failed read is
 * not reported as the page failing.
 */
export function chatProjectQueryOptions(projectId: number) {
  return {
    ...getProjectOptions({ path: { id: projectId } }),
    retry: false,
    staleTime: 60_000,
    meta: { routeFailure: ROUTE_FAILURE_IGNORE },
  }
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listS3SourcesOptions } from '@/api/client/@tanstack/react-query.gen'
import { ROUTE_FAILURE_IGNORE_REFUSAL } from '@/lib/route-query-failures'

/**
 * The list of backup destinations, as every component on a service page reads
 * it. They all share one cache entry, and the entry keeps the `meta` of
 * whichever observer configured it last, so each reader must carry the same
 * opt-out: a role that may run a backup but not list destinations is refused
 * here by design, and the page already explains it (a neutral note), so the
 * route banner must not repeat it. A server error or a dropped connection is
 * still reported.
 */
export function backupDestinationsOptions() {
  return {
    ...listS3SourcesOptions(),
    meta: { routeFailure: ROUTE_FAILURE_IGNORE_REFUSAL },
  }
}

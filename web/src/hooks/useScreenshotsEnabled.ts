// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getSettingsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'

/**
 * Whether the platform captures deployment screenshots.
 *
 * Reads the platform settings, which a role without `settings:read` is
 * refused. That is background context for the page, not something it waits
 * on, so the refusal is neither retried nor reported as a failure of the page,
 * and the answer is "off".
 */
export function useScreenshotsEnabled(): boolean {
  const { data: settings } = useQuery({
    ...getSettingsOptions(),
    retry: false,
    meta: { routeFailure: ROUTE_FAILURE_IGNORE },
  })
  return settings?.screenshots?.enabled ?? false
}

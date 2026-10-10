// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { getDiskStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { useIsInstanceAdmin } from './useIsInstanceAdmin'

/**
 * Hook to fetch current disk usage for the control-plane server.
 *
 * Returns live disk usage for the monitored path plus any disks that meet or
 * exceed the configured alert threshold. Read-only — never triggers
 * notifications. Used by the dashboard to surface a low-disk-space warning.
 *
 * The endpoint needs `settings:read` (instance administrators only). It is
 * polled from the app shell on every page, so for any other role it is never
 * requested: a refused poll would repeat every minute for as long as the tab
 * stays open.
 */
export function useDiskStatus() {
  const isAdmin = useIsInstanceAdmin()
  return useQuery({
    ...getDiskStatusOptions(),
    enabled: isAdmin,
    // Disk usage changes slowly; refresh in the background every 60s so the
    // dashboard banner reflects reality without hammering the endpoint.
    refetchInterval: 60_000,
    staleTime: 30_000,
    retry: false,
  })
}

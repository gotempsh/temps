import { useQuery } from '@tanstack/react-query'
import { getDiskStatusOptions } from '@/api/client/@tanstack/react-query.gen'

/**
 * Hook to fetch current disk usage for the control-plane server.
 *
 * Returns live disk usage for the monitored path plus any disks that meet or
 * exceed the configured alert threshold. Read-only — never triggers
 * notifications. Used by the dashboard to surface a low-disk-space warning.
 */
export function useDiskStatus() {
  return useQuery({
    ...getDiskStatusOptions(),
    // Disk usage changes slowly; refresh in the background every 60s so the
    // dashboard banner reflects reality without hammering the endpoint.
    refetchInterval: 60_000,
    staleTime: 30_000,
    retry: false,
  })
}

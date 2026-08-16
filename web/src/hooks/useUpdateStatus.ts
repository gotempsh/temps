import { useQuery } from '@tanstack/react-query'
import { getUpdateStatusOptions } from '@/api/client/@tanstack/react-query.gen'

/**
 * Hook to fetch the server's release-update status.
 *
 * The backend checks GitHub for a newer release on this install's channel
 * (stable/beta) shortly after startup and then daily, so this endpoint is
 * cheap — it only reads an in-memory slot. Drives the app-wide upgrade
 * banner.
 */
export function useUpdateStatus() {
  return useQuery({
    ...getUpdateStatusOptions(),
    // The server re-checks daily; hourly refetch keeps a long-lived tab
    // current without hammering the endpoint.
    refetchInterval: 60 * 60 * 1000,
    staleTime: 5 * 60 * 1000,
    retry: false,
  })
}

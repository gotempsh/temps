import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  checkForUpdateMutation,
  getUpdateCapabilityOptions,
  getUpdateCapabilityQueryKey,
  getUpdateStatusQueryKey,
  startUpdateMutation,
} from '@/api/client/@tanstack/react-query.gen'

/**
 * Whether this server can install a release on request, which version and
 * channel it is on, and how the last attempt went.
 *
 * Cheap (in-memory state plus one settings read), but not free, so it is only
 * fetched where the answer is actually used — pass `enabled: false` to skip it.
 * While an update is running, `pollMs` turns this into the progress feed: the
 * server reports `phase` as it downloads, verifies and installs.
 */
export function useSelfUpdateCapability({
  enabled = true,
  pollMs,
}: { enabled?: boolean; pollMs?: number } = {}) {
  return useQuery({
    ...getUpdateCapabilityOptions(),
    enabled,
    refetchInterval: pollMs,
    staleTime: pollMs ? 0 : 60 * 1000,
    // Deliberately keep retrying while polling: under automatic restart the
    // server is EXPECTED to stop answering mid-update, and giving up on the
    // first failure would strand the UI on "connection lost" exactly when the
    // user most needs to see the outcome.
    retry: pollMs ? true : false,
    retryDelay: 2000,
  })
}

/**
 * Start an update. Resolves as soon as the server accepts it — the install (and
 * the restart, where the server does that) happen afterwards, observable
 * through {@link useSelfUpdateCapability}.
 */
export function useStartSelfUpdate() {
  const queryClient = useQueryClient()

  return useMutation({
    ...startUpdateMutation(),
    onSuccess: () => {
      // The capability now reports a running attempt; refetch so polling starts
      // from the real phase rather than the stale idle snapshot.
      queryClient.invalidateQueries({ queryKey: getUpdateCapabilityQueryKey() })
    },
  })
}

/**
 * Query the release API immediately rather than waiting for the server's own
 * periodic check. Republishes the shared update notice, so the banner and the
 * version page never disagree.
 */
export function useCheckForUpdate() {
  const queryClient = useQueryClient()

  return useMutation({
    ...checkForUpdateMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: getUpdateStatusQueryKey() })
      queryClient.invalidateQueries({ queryKey: getUpdateCapabilityQueryKey() })
    },
  })
}

/** Drop the cached "update available" notice once the server is back. */
export function useInvalidateUpdateStatus() {
  const queryClient = useQueryClient()
  return () => {
    queryClient.invalidateQueries({ queryKey: getUpdateStatusQueryKey() })
    queryClient.invalidateQueries({ queryKey: getUpdateCapabilityQueryKey() })
  }
}

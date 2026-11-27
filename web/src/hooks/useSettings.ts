// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { getSandboxResourceDefaultsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import {
  getPlatformSettings,
  updatePlatformSettings as updateSettingsApi,
  type PlatformSettings,
} from '@/api/platformSettings'
import { problemDetail } from '@/lib/api-problem'
import { shouldRetryQuery } from '@/lib/query-retry'
import { isStaleRevision, STALE_REVISION_MESSAGE } from '@/lib/stale-revision'
import { useIsInstanceAdmin } from './useIsInstanceAdmin'

// Re-export types for backward compatibility
export type { PlatformSettings } from '@/api/platformSettings'
export type {
  DnsProviderSettings as DnsProvider,
  LetsEncryptSettings as LetsEncrypt,
  ScreenshotSettings as Screenshots,
} from '@/api/client/types.gen'

/**
 * Platform settings (`GET /settings`). The endpoint needs `settings:read`,
 * which only instance administrators hold, so the read is never sent for
 * any other role: it would be refused every time, from every page that
 * mounts this hook. For those roles the query stays idle with no data.
 */
export function useSettings(options: { enabled?: boolean } = {}) {
  const isAdmin = useIsInstanceAdmin()
  return useQuery({
    queryKey: ['platform-settings'],
    queryFn: getPlatformSettings,
    staleTime: 5 * 60 * 1000, // 5 minutes
    // A refused read stays refused; anything else gets one more try.
    retry: (failureCount, error) =>
      failureCount < 1 && shouldRetryQuery(failureCount, error),
    enabled: isAdmin && (options.enabled ?? true),
  })
}

/**
 * What the server answers a role without `settings:read`. The settings pages
 * show this instead of the form: the read is never sent for such a role, so
 * without it the form would be filled with the built-in defaults and look like
 * the instance's configuration.
 */
export const SETTINGS_REFUSAL = {
  status: 403,
  title: 'Forbidden',
  detail:
    'Reading platform settings requires the settings:read permission, which your role does not include.',
}

/**
 * `useSettings` for a page that renders the settings as a form. A role that
 * cannot read them gets a forbidden `error` right away, so the page's error
 * branch runs instead of falling through to the form with its defaults.
 */
export function useSettingsPage() {
  const isAdmin = useIsInstanceAdmin()
  const query = useSettings()
  return {
    ...query,
    error: isAdmin ? query.error : SETTINGS_REFUSAL,
  }
}

/**
 * Re-read the settings after a save was refused as stale, and hand the
 * stored document to `onLatestLoaded` (rejecting, without calling it, when
 * the read fails) (a form that keeps its unsaved edits
 * across refetches adopts its revision, so saving again applies the edits
 * on top of the other save).
 */
export async function loadLatestSettings(
  queryClient: ReturnType<typeof useQueryClient>,
  onLatestLoaded?: (latest: PlatformSettings) => void
) {
  // A failed read rejects instead of leaving the old document in the cache
  // to be adopted as if it were the latest.
  await queryClient.refetchQueries(
    { queryKey: ['platform-settings'], exact: true },
    { throwOnError: true }
  )
  const latest = queryClient.getQueryData<PlatformSettings>([
    'platform-settings',
  ])
  if (latest) onLatestLoaded?.(latest)
}

/**
 * `baseRevision` is the revision the page's form values are based on
 * (`useFormBaseRevision`); a form that merges refetches into unsaved edits
 * passes it, so a refetch does not make a save of those edits unconditional.
 * Without it, the revision of the cached read is sent. `onLatestLoaded`
 * runs when the operator loads the latest settings after a refused save.
 */
export function useUpdateSettings(
  options: {
    baseRevision?: string
    onLatestLoaded?: (latest: PlatformSettings) => void
  } = {}
) {
  const queryClient = useQueryClient()
  const loadLatest = async () => {
    try {
      await loadLatestSettings(queryClient, options.onLatestLoaded)
    } catch (error) {
      // The form and the revision its edits are based on stay as they were.
      toast.error('Could not load the latest settings', {
        description: `${problemDetail(error, 'The server did not answer.')} Your unsaved changes are kept.`,
        action: { label: 'Retry', onClick: () => void loadLatest() },
      })
    }
  }

  return useMutation({
    // The revision the page was rendered from, not a fresh read: a save made
    // by someone else since this page loaded is refused instead of reverted.
    mutationFn: (changes: Partial<PlatformSettings>) =>
      updateSettingsApi(changes, {
        expectedRevision:
          options.baseRevision ??
          queryClient.getQueryData<PlatformSettings>(['platform-settings'])
            ?.revision,
      }),
    onSuccess: (data, changes) => {
      if (changes.agent_sandbox) {
        void queryClient.invalidateQueries({
          queryKey: getSandboxResourceDefaultsQueryKey(),
        })
      }
      queryClient.setQueryData(['platform-settings'], data)
      queryClient.invalidateQueries({ queryKey: ['platform-settings'] })
    },
    onError: (error) => {
      if (isStaleRevision(error)) {
        // Re-read in place, not a page reload: a form that keeps unsaved
        // edits across refetches keeps them, shows the other save in the
        // fields it did not touch, and adopts its revision.
        toast.error('Settings were changed by someone else', {
          description: STALE_REVISION_MESSAGE,
          action: {
            label: 'Load latest',
            onClick: () => void loadLatest(),
          },
        })
        return
      }
      toast.error('Failed to update settings', {
        description: error instanceof Error ? error.message : 'Unknown error',
      })
    },
  })
}

// Export for backwards compatibility
export type Settings = PlatformSettings

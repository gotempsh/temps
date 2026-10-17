// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  getPlatformSettings,
  updatePlatformSettings as updateSettingsApi,
  type PlatformSettings,
} from '@/api/platformSettings'
import { shouldRetryQuery } from '@/lib/query-retry'
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

export function useUpdateSettings() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: updateSettingsApi,
    onSuccess: (data) => {
      queryClient.setQueryData(['platform-settings'], data)
      queryClient.invalidateQueries({ queryKey: ['platform-settings'] })
    },
    onError: (error) => {
      toast.error('Failed to update settings', {
        description: error instanceof Error ? error.message : 'Unknown error',
      })
    },
  })
}

// Export for backwards compatibility
export type Settings = PlatformSettings

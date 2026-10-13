// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  getPlatformSettings,
  updatePlatformSettings as updateSettingsApi,
  type PlatformSettings,
} from '@/api/platformSettings'
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
    retry: 1,
    enabled: isAdmin && (options.enabled ?? true),
  })
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

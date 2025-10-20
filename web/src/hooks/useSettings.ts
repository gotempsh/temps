import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  getPlatformSettings,
  updatePlatformSettings as updateSettingsApi,
  type PlatformSettings,
} from '@/api/platformSettings'

// Re-export types for backward compatibility
export type { PlatformSettings } from '@/api/platformSettings'
export type {
  DnsProviderSettings as DnsProvider,
  LetsEncryptSettings as LetsEncrypt,
  ScreenshotSettings as Screenshots,
} from '@/api/client/types.gen'

export function useSettings() {
  return useQuery({
    queryKey: ['platform-settings'],
    queryFn: getPlatformSettings,
    staleTime: 5 * 60 * 1000, // 5 minutes
    retry: 1,
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

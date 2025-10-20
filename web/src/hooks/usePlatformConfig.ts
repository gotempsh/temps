import { useQuery } from '@tanstack/react-query'
import {
  getDnsProvider,
  getExternalUrl,
  getLetsEncryptConfig,
  getPreviewDomain,
  getScreenshotsConfig,
} from '@/api/platformSettings'

/**
 * Convenience hooks for accessing specific platform configuration values
 * These hooks provide easy access to individual settings without needing the full settings object
 */

/**
 * Hook to get DNS provider configuration
 */
export function useDnsProvider() {
  return useQuery({
    queryKey: ['platform-settings', 'dns-provider'],
    queryFn: getDnsProvider,
    staleTime: 5 * 60 * 1000,
  })
}

/**
 * Hook to get external URL configuration
 */
export function useExternalUrl() {
  return useQuery({
    queryKey: ['platform-settings', 'external-url'],
    queryFn: getExternalUrl,
    staleTime: 5 * 60 * 1000,
  })
}

/**
 * Hook to get Let's Encrypt configuration
 */
export function useLetsEncrypt() {
  return useQuery({
    queryKey: ['platform-settings', 'letsencrypt'],
    queryFn: getLetsEncryptConfig,
    staleTime: 5 * 60 * 1000,
  })
}

/**
 * Hook to get preview domain configuration
 */
export function usePreviewDomain() {
  return useQuery({
    queryKey: ['platform-settings', 'preview-domain'],
    queryFn: getPreviewDomain,
    staleTime: 5 * 60 * 1000,
  })
}

/**
 * Hook to get screenshots configuration
 */
export function useScreenshots() {
  return useQuery({
    queryKey: ['platform-settings', 'screenshots'],
    queryFn: getScreenshotsConfig,
    staleTime: 5 * 60 * 1000,
  })
}

/**
 * Hook to check if platform is properly configured
 * Returns true if essential settings are configured
 */
export function usePlatformConfigured() {
  const { data: externalUrl } = useExternalUrl()
  const { data: letsencrypt } = useLetsEncrypt()

  return {
    isConfigured: !!(externalUrl && letsencrypt?.email),
    missingConfigs: [
      !externalUrl && 'External URL',
      !letsencrypt?.email && "Let's Encrypt Email",
    ].filter(Boolean) as string[],
  }
}

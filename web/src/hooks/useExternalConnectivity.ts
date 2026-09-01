// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { listDomainsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useSettings } from './useSettings'

/**
 * Hook to check if external connectivity is properly configured
 * Checks for:
 * - Wildcard domain configuration
 * - External URL setting
 * - Public accessibility (if possible)
 * - Whether user is accessing via HTTPS domain (auto-configured)
 */
export function useExternalConnectivity() {
  const { data: settings } = useSettings()
  const { data: wildcardData } = useQuery({
    ...listDomainsOptions({
      query: { search: '*.', page_size: 100 },
    }),
    retry: false,
  })

  // Check if user is accessing via HTTPS with a domain (not IP)
  const isAccessingViaDomain = () => {
    const currentUrl = window.location.hostname
    // Check if HTTPS and not an IP address
    const isHttps = window.location.protocol === 'https:'
    const isIpAddress =
      /^(\d{1,3}\.){3}\d{1,3}$/.test(currentUrl) ||
      /^localhost$/.test(currentUrl) ||
      /^127\.0\.0\.1$/.test(currentUrl) ||
      /^\[.*\]$/.test(currentUrl) // IPv6
    return isHttps && !isIpAddress
  }

  // Filter to actual wildcard domains (server search is substring match)
  const wildcardDomains =
    wildcardData?.domains?.filter((d) => d.domain.startsWith('*.')) || []

  // Check if we have a wildcard domain
  const hasWildcardDomain = wildcardDomains.length > 0

  // Check if external URL is configured
  const hasExternalUrl = !!settings?.external_url

  // If accessing via HTTPS domain, consider it configured
  const isAccessingViaHttpsDomain = isAccessingViaDomain()

  // Determine overall configuration status
  const isConfigured =
    (hasWildcardDomain && hasExternalUrl) || isAccessingViaHttpsDomain

  // Determine what's missing
  const missingConfigs: string[] = []
  if (!hasWildcardDomain && !isAccessingViaHttpsDomain)
    missingConfigs.push('Wildcard Domain')
  if (!hasExternalUrl && !isAccessingViaHttpsDomain)
    missingConfigs.push('External URL')

  return {
    isConfigured,
    hasWildcardDomain,
    hasExternalUrl,
    isAccessingViaHttpsDomain,
    missingConfigs,
    domains: wildcardData?.domains || [],
    wildcardDomains,
  }
}

/**
 * Hook to determine if the user needs external connectivity setup
 * This checks if they have projects but missing external connectivity
 */
export function useNeedsConnectivitySetup() {
  const { isConfigured } = useExternalConnectivity()

  // For now, just return if not configured
  // Could be enhanced to check if user has projects first
  return !isConfigured
}

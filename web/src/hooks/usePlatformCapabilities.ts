// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { usePlatformAccess } from '@/contexts/PlatformAccessContext'

/**
 * How this browser reached the server, and the server's addresses.
 *
 * The access mode describes this one request, never which domains can be
 * added: a console opened through a Cloudflare Tunnel still adds domains
 * served with Temps certificates. Which ways a domain can get HTTPS is
 * judged per domain by `GET /ingress/certificate-methods?domain=`.
 */
export function usePlatformCapabilities() {
  const { accessInfo, isLoading, error } = usePlatformAccess()

  // Expose localMode as a boolean
  const localMode = accessInfo?.access_mode === 'local'

  // The address DNS records point at for a domain served directly.
  const getDNSTargetIP = (): string | null => {
    // Opened directly or behind NAT: the detected public IP.
    if (
      accessInfo?.access_mode === 'direct' ||
      accessInfo?.access_mode === 'nat'
    ) {
      return accessInfo?.public_ip || null
    }
    // Opened through Cloudflare or locally, this request does not show how
    // the internet reaches the server; the helper says so where it applies.
    return null
  }

  // Check if the platform has a public IP available
  const hasPublicIP = (): boolean => {
    return !!accessInfo?.public_ip
  }

  // Check if the platform needs port forwarding configuration
  const needsPortForwarding = (): boolean => {
    return accessInfo?.access_mode === 'nat'
  }

  // This request came through Cloudflare's edge (a tunnel or its proxy).
  const isOpenedThroughCloudflare = (): boolean => {
    return accessInfo?.access_mode === 'cloudflare_tunnel'
  }

  // Check if the platform is in local development mode
  const isLocalMode = (): boolean => {
    return accessInfo?.access_mode === 'local'
  }

  return {
    // Access mode info
    accessMode: accessInfo?.access_mode,
    publicIP: accessInfo?.public_ip,
    privateIP: accessInfo?.private_ip,
    isLoading,
    error,

    localMode, // Expose localMode as boolean

    // Helper functions
    getDNSTargetIP,
    hasPublicIP,
    needsPortForwarding,
    isOpenedThroughCloudflare,
    isLocalMode,
  }
}

import { usePlatformAccess } from '@/contexts/PlatformAccessContext'

/**
 * Hook that determines platform capabilities based on the access mode
 * Used to restrict or enable features based on how the platform is accessed
 */
export function usePlatformCapabilities() {
  const { accessInfo, isLoading, error } = usePlatformAccess()

  // Determine if the platform can manage SSL certificates
  // Cloudflare tunnel handles certificates automatically, so manual management is disabled
  const canManageCertificates = accessInfo?.access_mode !== 'cloudflare_tunnel'

  // Determine if the platform can create custom domains
  // Cloudflare tunnel requires domain configuration through Cloudflare dashboard
  const canCreateDomains = accessInfo?.access_mode !== 'cloudflare_tunnel'

  // Expose localMode as a boolean
  const localMode = accessInfo?.access_mode === 'local'

  // Get the IP address that should be used for DNS records
  const getDNSTargetIP = (): string | null => {
    // For direct access or NAT, use the public IP
    if (
      accessInfo?.access_mode === 'direct' ||
      accessInfo?.access_mode === 'nat'
    ) {
      return accessInfo?.public_ip || null
    }
    // Cloudflare tunnel doesn't need IP configuration (handled by Cloudflare)
    // Local mode doesn't have a public IP
    return null
  }

  // Get context-specific warning or instruction message
  const getAccessModeWarning = (): string | null => {
    switch (accessInfo?.access_mode) {
      case 'cloudflare_tunnel':
        return 'SSL certificates and domains are managed by Cloudflare. Use Cloudflare dashboard for configuration.'
      case 'nat':
        return accessInfo?.public_ip
          ? `Point DNS A records to ${accessInfo.public_ip}. Ensure ports 80/443 are forwarded correctly.`
          : 'NAT mode detected. Ensure ports 80/443 are forwarded correctly.'
      case 'direct':
        return accessInfo?.public_ip
          ? `Point DNS A records to ${accessInfo.public_ip}`
          : 'Direct access mode detected.'
      case 'local':
        return 'Running in local mode. Configure external access for domain and certificate management.'
      default:
        return null
    }
  }

  // Get instructions for DNS configuration
  const getDNSInstructions = (): {
    type: 'info' | 'warning' | 'success' | 'error'
    title: string
    message: string
  } | null => {
    if (!accessInfo) return null

    switch (accessInfo.access_mode) {
      case 'cloudflare_tunnel':
        return {
          type: 'info',
          title: 'Cloudflare Tunnel Active',
          message:
            'Domains and SSL certificates are automatically managed by Cloudflare. Configure domains through your Cloudflare dashboard.',
        }
      case 'nat':
        return {
          type: 'warning',
          title: 'NAT Configuration Required',
          message: accessInfo.public_ip
            ? `Point your DNS A records to ${accessInfo.public_ip}. Ensure ports 80 and 443 are properly forwarded from your router to this server.`
            : 'Configure port forwarding for ports 80 and 443 from your router to this server.',
        }
      case 'direct':
        return {
          type: 'success',
          title: 'Direct Access Available',
          message: accessInfo.public_ip
            ? `Point your DNS A records to ${accessInfo.public_ip}.`
            : 'Direct access mode is active.',
        }
      case 'local':
        return {
          type: 'warning',
          title: 'Local Development Mode',
          message:
            'Configure external access (port forwarding or tunnel) to enable domain and certificate management.',
        }
      default:
        return null
    }
  }

  // Check if the platform has a public IP available
  const hasPublicIP = (): boolean => {
    return !!accessInfo?.public_ip
  }

  // Check if the platform needs port forwarding configuration
  const needsPortForwarding = (): boolean => {
    return accessInfo?.access_mode === 'nat'
  }

  // Check if the platform is using Cloudflare tunnel
  const isUsingCloudflare = (): boolean => {
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

    // Capabilities
    canManageCertificates,
    canCreateDomains,
    localMode, // Expose localMode as boolean

    // Helper functions
    getDNSTargetIP,
    getAccessModeWarning,
    getDNSInstructions,
    hasPublicIP,
    needsPortForwarding,
    isUsingCloudflare,
    isLocalMode,
  }
}

import { usePlatformAccess as usePlatformAccessContext } from '@/contexts/PlatformAccessContext'

/**
 * Helper hook to get just the access mode
 */
export function useAccessMode() {
  const { accessInfo } = usePlatformAccessContext()
  return accessInfo?.access_mode
}

/**
 * Helper hook to check if we're running in local mode
 */
export function useIsLocalMode() {
  const { isLocal } = usePlatformAccessContext()
  return isLocal
}

/**
 * Helper hook to check if we're behind NAT
 */
export function useIsNatMode() {
  const { isNat } = usePlatformAccessContext()
  return isNat
}

/**
 * Helper hook to check if we're using Cloudflare tunnel
 */
export function useIsCloudflareMode() {
  const { isCloudflare } = usePlatformAccessContext()
  return isCloudflare
}

/**
 * Helper hook to check if we have direct access
 */
export function useIsDirectMode() {
  const { isDirect } = usePlatformAccessContext()
  return isDirect
}

/**
 * Re-export the main usePlatformAccess hook for convenience
 */
export { usePlatformAccess } from '@/contexts/PlatformAccessContext'

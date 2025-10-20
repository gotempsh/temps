import { createContext, useContext, ReactNode, useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getAccessInfoOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ServiceAccessInfo } from '@/api/client/types.gen'

interface PlatformAccessContextValue {
  accessInfo: ServiceAccessInfo | undefined
  isLoading: boolean
  error: Error | null
  refetch: () => void
  isLocal: boolean
  isNat: boolean
  isCloudflare: boolean
  isDirect: boolean
}

const PlatformAccessContext = createContext<
  PlatformAccessContextValue | undefined
>(undefined)

interface PlatformAccessProviderProps {
  children: ReactNode
}

export function PlatformAccessProvider({
  children,
}: PlatformAccessProviderProps) {
  const {
    data: accessInfo,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getAccessInfoOptions(),
    // Cache for 5 minutes since this info doesn't change frequently
    staleTime: 5 * 60 * 1000,
    // Retry on error since this is critical infrastructure info
    retry: 3,
    // Refetch when window focus returns in case network conditions changed
    refetchOnWindowFocus: true,
  })

  // Compute boolean flags for access modes
  const isLocal = accessInfo?.access_mode === 'local'
  const isNat = accessInfo?.access_mode === 'nat'
  const isCloudflare = accessInfo?.access_mode === 'cloudflare_tunnel'
  const isDirect = accessInfo?.access_mode === 'direct'

  // Handle platform-specific side effects
  useEffect(() => {
    if (accessInfo) {
      // Store in window for debugging purposes
      if (typeof window !== 'undefined') {
        ;(window as any).__PLATFORM_ACCESS__ = accessInfo
      }
    }
  }, [accessInfo])

  // Manage body classes based on access mode
  useEffect(() => {
    const bodyClassList = document.body.classList

    // Remove all platform classes first
    bodyClassList.remove(
      'platform-local',
      'platform-cloudflare',
      'platform-nat',
      'platform-direct'
    )

    // Add the appropriate class based on access mode
    if (isLocal) {
      bodyClassList.add('platform-local')
    } else if (isCloudflare) {
      bodyClassList.add('platform-cloudflare')
    } else if (isNat) {
      bodyClassList.add('platform-nat')
    } else if (isDirect) {
      bodyClassList.add('platform-direct')
    }

    // Cleanup function
    return () => {
      bodyClassList.remove(
        'platform-local',
        'platform-cloudflare',
        'platform-nat',
        'platform-direct'
      )
    }
  }, [isLocal, isCloudflare, isNat, isDirect])

  const value: PlatformAccessContextValue = {
    accessInfo,
    isLoading,
    error: error as Error | null,
    refetch,
    isLocal,
    isNat,
    isCloudflare,
    isDirect,
  }

  return (
    <PlatformAccessContext.Provider value={value}>
      {children}
    </PlatformAccessContext.Provider>
  )
}

export function usePlatformAccess() {
  const context = useContext(PlatformAccessContext)
  if (!context) {
    throw new Error(
      'usePlatformAccess must be used within a PlatformAccessProvider'
    )
  }
  return context
}

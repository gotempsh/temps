// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, ReactNode } from 'react'
import type { ServiceAccessInfo } from '@/api/client/types.gen'

export interface PlatformAccessContextValue {
  accessInfo: ServiceAccessInfo | undefined
  isLoading: boolean
  error: Error | null
  refetch: () => void
  isLocal: boolean
  isNat: boolean
  isCloudflare: boolean
  isDirect: boolean
}

export const PlatformAccessContext = createContext<
  PlatformAccessContextValue | undefined
>(undefined)

export interface PlatformAccessProviderProps {
  children: ReactNode
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

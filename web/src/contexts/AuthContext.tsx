// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getCurrentUserOptions,
  logoutMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { UserResponse } from '@/api/client/types.gen'
import { useMutation, useQuery } from '@tanstack/react-query'
import { createContext, useContext, ReactNode } from 'react'

interface AuthContextType {
  user: UserResponse | null
  isLoading: boolean
  error: Error | null
  logout: () => Promise<void>
  refetch: () => void
}

const AuthContext = createContext<AuthContextType | undefined>(undefined)

export function AuthProvider({ children }: { children: ReactNode }) {
  const {
    data: user,
    isLoading: userLoading,
    error: userError,
    dataUpdatedAt,
    errorUpdatedAt,
    refetch: refetchUser,
  } = useQuery({
    ...getCurrentUserOptions({}),
    retry: (failureCount, error: any) => {
      // Don't retry on 401 (unauthorized) or cancelled requests
      if (error?.status === 401 || error?.name === 'AbortError') {
        return false
      }
      // Don't retry on 504 or connection errors
      if (
        error?.status === 504 ||
        error?.code === 'ECONNREFUSED' ||
        error?.message?.includes('Failed to fetch')
      ) {
        return false
      }
      return failureCount < 1
    },
    retryDelay: 100,
    staleTime: 1000 * 60 * 5, // Consider data stale after 5 minutes
    gcTime: 1000 * 60 * 10, // Keep in cache for 10 minutes
  })

  // `getCurrentUser` never holds data while logged out, so TanStack Query
  // resets its status to 'pending' (userLoading -> true) on every refetch,
  // not just the first one -- including refetches an unrelated query
  // triggers by invalidating this query's key after a 401 of its own (see
  // App.tsx's QueryCache.onError). ProtectedLayout renders a full-screen
  // spinner while `isLoading` is true, which unmounts and remounts <Login />
  // -- wiping any in-progress email/password input. Only the very FIRST
  // resolution (success or error) should count as "loading"; every
  // subsequent refetch should be invisible to consumers already showing the
  // login screen. `dataUpdatedAt`/`errorUpdatedAt` are timestamps TanStack
  // Query itself maintains on the query (0 until it has settled once), so
  // this is a plain derivation of query state every render rather than a
  // second state machine that has to be kept in sync with it.
  const hasResolvedOnce = dataUpdatedAt > 0 || errorUpdatedAt > 0

  const { mutateAsync: logout } = useMutation({
    ...logoutMutation({}),
    meta: {
      errorTitle: 'Failed to logout',
    },
    onSuccess: () => {
      window.location.reload()
    },
  })

  const value = {
    user: user || null,
    isLoading: userLoading && !hasResolvedOnce,
    error: userError as Error | null,
    logout: async () => {
      await logout({})
    },
    refetch: refetchUser,
  }

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth() {
  const context = useContext(AuthContext)
  if (context === undefined) {
    throw new Error('useAuth must be used within an AuthProvider')
  }
  return context
}

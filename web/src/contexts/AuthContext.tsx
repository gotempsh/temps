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
  /** True when logged in as demo user with limited access (via demo.<domain> subdomain) */
  isDemoMode: boolean
}

const AuthContext = createContext<AuthContextType | undefined>(undefined)

export function AuthProvider({ children }: { children: ReactNode }) {
  const {
    data: user,
    isLoading: userLoading,
    error: userError,
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

  const { mutateAsync: logout } = useMutation({
    ...logoutMutation({}),
    meta: {
      errorTitle: 'Failed to logout',
    },
    onSuccess: () => {
      window.location.reload()
    },
  })

  // Check if user is in demo mode based on their role
  // Demo mode is triggered by accessing demo.<preview_domain> subdomain
  // The proxy injects X-Temps-Demo-Mode header and the backend auto-authenticates as demo user
  const isDemoMode = user?.role === 'demo'

  const value = {
    user: user || null,
    isLoading: userLoading,
    error: userError as Error | null,
    logout: async () => {
      await logout({})
    },
    refetch: refetchUser,
    isDemoMode,
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

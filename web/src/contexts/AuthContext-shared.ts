// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { UserResponse } from '@/api/client/types.gen'
import { createContext, useContext } from 'react'

export interface AuthContextType {
  user: UserResponse | null
  isLoading: boolean
  error: Error | null
  logout: () => Promise<void>
  refetch: () => void
}

export const AuthContext = createContext<AuthContextType | undefined>(undefined)

export function useAuth() {
  const context = useContext(AuthContext)
  if (context === undefined) {
    throw new Error('useAuth must be used within an AuthProvider')
  }
  return context
}

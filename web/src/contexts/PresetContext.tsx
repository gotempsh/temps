// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { listPresets } from '@/api/client'
import type { PresetResponse } from '@/api/client'
import { useAuth } from '@/contexts/AuthContext'

interface PresetContextType {
  presets: PresetResponse[]
  isLoading: boolean
  error: Error | null
  getPresetBySlug: (slug: string) => PresetResponse | undefined
}

const PresetContext = createContext<PresetContextType | undefined>(undefined)

export function PresetProvider({ children }: { children: ReactNode }) {
  const { user } = useAuth()
  const { data, isLoading, error } = useQuery({
    // Scoped to `user?.id` for the same reason as ProjectsContext's query --
    // the generated `listPresetsOptions()` key carries no identity, and the
    // 1-hour staleTime below means a re-enabled query on an identity-
    // independent key could serve a previous account's presets from cache
    // without ever refetching after a different account signs in without a
    // full page reload. Each account gets its own cache slot instead.
    queryKey: ['listPresets', user?.id] as const,
    queryFn: async ({ signal }) => {
      const { data } = await listPresets({ signal, throwOnError: true })
      return data
    },
    staleTime: 1000 * 60 * 60, // Cache for 1 hour
    gcTime: 1000 * 60 * 60 * 24, // Keep in cache for 24 hours
    // PresetProvider wraps the whole app, including the logged-out login
    // screen -- see the matching comment in ProjectsContext.tsx for why this
    // must not fire while logged out.
    enabled: !!user,
  })

  const presets = data?.presets || []

  const getPresetBySlug = (slug: string) => {
    return presets.find((preset) => preset.slug === slug)
  }

  return (
    <PresetContext.Provider
      value={{
        presets,
        isLoading,
        error: error as Error | null,
        getPresetBySlug,
      }}
    >
      {children}
    </PresetContext.Provider>
  )
}

export function usePresets() {
  const context = useContext(PresetContext)
  if (context === undefined) {
    throw new Error('usePresets must be used within a PresetProvider')
  }
  return context
}

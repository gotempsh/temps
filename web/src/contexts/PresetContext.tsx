import { createContext, useContext, ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { listPresetsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { PresetResponse } from '@/api/client'

interface PresetContextType {
  presets: PresetResponse[]
  isLoading: boolean
  error: Error | null
  getPresetBySlug: (slug: string) => PresetResponse | undefined
}

const PresetContext = createContext<PresetContextType | undefined>(undefined)

export function PresetProvider({ children }: { children: ReactNode }) {
  const { data, isLoading, error } = useQuery({
    ...listPresetsOptions(),
    staleTime: 1000 * 60 * 60, // Cache for 1 hour
    gcTime: 1000 * 60 * 60 * 24, // Keep in cache for 24 hours
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'
import type { PresetResponse } from '@/api/client'

export interface PresetContextType {
  presets: PresetResponse[]
  isLoading: boolean
  error: Error | null
  getPresetBySlug: (slug: string) => PresetResponse | undefined
}

export const PresetContext = createContext<PresetContextType | undefined>(
  undefined
)

export function usePresets() {
  const context = useContext(PresetContext)
  if (context === undefined) {
    throw new Error('usePresets must be used within a PresetProvider')
  }
  return context
}

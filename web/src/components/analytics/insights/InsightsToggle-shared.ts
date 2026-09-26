// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState } from 'react'

/**
 * Shared show/hide preference for the analytics insights panels. One key
 * for all analytics pages: opening insights on one page opens them
 * everywhere, since it expresses a single "I want insights" preference.
 */
export const STORAGE_KEY = 'temps.analytics.insights.open'

export function useInsightsOpen(): [boolean, (open: boolean) => void] {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(STORAGE_KEY) === 'true'
    } catch {
      return false
    }
  })
  const update = useCallback((next: boolean) => {
    setOpen(next)
    try {
      localStorage.setItem(STORAGE_KEY, String(next))
    } catch {
      // Preference just won't persist — toggling still works this session.
    }
  }, [])
  return [open, update]
}

export interface InsightsToggleButtonProps {
  open: boolean
  onToggle: (open: boolean) => void
}

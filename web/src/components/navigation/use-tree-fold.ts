// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState } from 'react'

/**
 * Which rows of the sidebar's tree the user has opened or folded. A row
 * they never touched follows the page: open while what it holds is shown,
 * folded otherwise. Once touched, their choice holds across pages and
 * visits.
 */
export const TREE_FOLD_KEY = 'temps:sidebar:tree'

type Folds = Record<string, boolean>

function read(): Folds {
  try {
    const raw = window.localStorage.getItem(TREE_FOLD_KEY)
    const parsed: unknown = raw ? JSON.parse(raw) : {}
    return parsed && typeof parsed === 'object' ? (parsed as Folds) : {}
  } catch {
    // Storage unavailable or corrupt: every row follows the page.
    return {}
  }
}

export interface TreeFold {
  /** Whether `key` is open, given whether it opens when left untouched. */
  isOpen: (key: string, untouched: boolean) => boolean
  setOpen: (key: string, open: boolean) => void
}

export function useTreeFold(): TreeFold {
  const [folds, setFolds] = useState(read)
  const setOpen = useCallback((key: string, open: boolean) => {
    setFolds((current) => {
      const next = { ...current, [key]: open }
      try {
        window.localStorage.setItem(TREE_FOLD_KEY, JSON.stringify(next))
      } catch {
        // Not remembered across visits, but still applied now.
      }
      return next
    })
  }, [])
  const isOpen = useCallback(
    (key: string, untouched: boolean) => folds[key] ?? untouched,
    [folds]
  )
  return { isOpen, setOpen }
}

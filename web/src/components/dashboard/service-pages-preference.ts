// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState } from 'react'

/**
 * Whether the open service's pages are listed under it in a project's
 * sidebar. Off until the user turns it on, so the project's own pages stay
 * in view; once on, it stays on as they move between services and visits.
 */
export const SERVICE_PAGES_KEY = 'temps:sidebar:service-pages'

function read(): boolean {
  try {
    return window.localStorage.getItem(SERVICE_PAGES_KEY) === 'shown'
  } catch {
    // Storage unavailable (private mode, blocked): fall back to the default.
    return false
  }
}

export function useServicePagesShown(): [boolean, (shown: boolean) => void] {
  const [shown, setShown] = useState(read)
  const set = useCallback((next: boolean) => {
    setShown(next)
    try {
      if (next) window.localStorage.setItem(SERVICE_PAGES_KEY, 'shown')
      else window.localStorage.removeItem(SERVICE_PAGES_KEY)
    } catch {
      // Not remembered across visits, but still applied now.
    }
  }, [])
  return [shown, set]
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { withRecent } from '@/lib/entity-tree'

/**
 * The projects opened recently in this browser, most recent first, by tree
 * key. On a server with many projects the tree lists these after the ones
 * in trouble. Remembered per browser: nothing is sent to the server.
 */
export const RECENT_PROJECTS_KEY = 'temps:sidebar:recent-projects'

function read(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENT_PROJECTS_KEY)
    const parsed: unknown = raw ? JSON.parse(raw) : []
    return Array.isArray(parsed)
      ? parsed.filter((key): key is string => typeof key === 'string')
      : []
  } catch {
    // Storage unavailable or corrupt: nothing is remembered yet.
    return []
  }
}

/** The recent projects, with `current` (the one shown) recorded first. */
export function useRecentProjects(current: string | undefined): string[] {
  const [seen, setSeen] = useState(() => {
    const known = read()
    return { current, recent: current ? withRecent(known, current) : known }
  })
  // Another project shown: recorded as it renders, not an effect later.
  if (current && current !== seen.current)
    setSeen({ current, recent: withRecent(seen.recent, current) })
  useEffect(() => {
    try {
      window.localStorage.setItem(
        RECENT_PROJECTS_KEY,
        JSON.stringify(seen.recent)
      )
    } catch {
      // Not remembered across visits, but still applied now.
    }
  }, [seen.recent])
  return seen.recent
}

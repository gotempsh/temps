// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  parsePinnedProjectTools,
  pinnedProjectToolsStorageKey,
  PINNED_PROJECT_TOOLS_CHANGED,
} from '@/lib/pinned-project-tools'
import { useCallback, useMemo, useSyncExternalStore } from 'react'

export function usePinnedProjectTools(
  projectSlug: string,
  allowedToolUrls: string[]
) {
  const allowedKey = allowedToolUrls.join('\u0000')
  const allowedUrls = useMemo(
    () => allowedKey.split('\u0000').filter(Boolean),
    [allowedKey]
  )
  const storageKey = pinnedProjectToolsStorageKey(projectSlug)

  const subscribe = useCallback(
    (onStoreChange: () => void) => {
      const sync = (event: Event) => {
        if (
          event instanceof StorageEvent &&
          event.key !== null &&
          event.key !== storageKey
        )
          return
        if (
          event instanceof CustomEvent &&
          event.detail !== undefined &&
          event.detail !== storageKey
        )
          return
        onStoreChange()
      }

      window.addEventListener('storage', sync)
      window.addEventListener(PINNED_PROJECT_TOOLS_CHANGED, sync)
      return () => {
        window.removeEventListener('storage', sync)
        window.removeEventListener(PINNED_PROJECT_TOOLS_CHANGED, sync)
      }
    },
    [storageKey]
  )

  const getSnapshot = useCallback(
    () => localStorage.getItem(storageKey) ?? '',
    [storageKey]
  )
  const rawPinnedUrls = useSyncExternalStore(subscribe, getSnapshot, () => '')
  const pinnedUrls = useMemo(
    () => parsePinnedProjectTools(rawPinnedUrls, allowedUrls),
    [allowedUrls, rawPinnedUrls]
  )

  const togglePinned = useCallback(
    (url: string) => {
      if (!allowedUrls.includes(url)) return
      const current = parsePinnedProjectTools(getSnapshot(), allowedUrls)
      const next = current.includes(url)
        ? current.filter((candidate) => candidate !== url)
        : [...current, url]
      localStorage.setItem(storageKey, JSON.stringify(next))
      window.dispatchEvent(
        new CustomEvent(PINNED_PROJECT_TOOLS_CHANGED, { detail: storageKey })
      )
    },
    [allowedUrls, getSnapshot, storageKey]
  )

  return {
    pinnedUrls,
    pinnedSet: useMemo(() => new Set(pinnedUrls), [pinnedUrls]),
    togglePinned,
  }
}

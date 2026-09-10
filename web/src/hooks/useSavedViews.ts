// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useState } from 'react'
import {
  deleteView,
  listViews,
  makeViewId,
  SavedView,
  touchView,
  upsertView,
} from '@/lib/data-browser-views'

const CHANGE_EVENT = 'temps:data-browser:views:change'

function emitChange(serviceId: string) {
  if (typeof window === 'undefined') return
  window.dispatchEvent(
    new CustomEvent(CHANGE_EVENT, { detail: { serviceId } })
  )
}

export function useSavedViews(serviceId: string) {
  const [views, setViews] = useState<SavedView[]>(() => listViews(serviceId))

  useEffect(() => {
    setViews(listViews(serviceId))
    const onChange = (e: Event) => {
      const detail = (e as CustomEvent<{ serviceId: string }>).detail
      if (detail?.serviceId === serviceId) {
        setViews(listViews(serviceId))
      }
    }
    const onStorage = (e: StorageEvent) => {
      if (e.key && e.key.startsWith('temps:data-browser:views')) {
        setViews(listViews(serviceId))
      }
    }
    window.addEventListener(CHANGE_EVENT, onChange)
    window.addEventListener('storage', onStorage)
    return () => {
      window.removeEventListener(CHANGE_EVENT, onChange)
      window.removeEventListener('storage', onStorage)
    }
  }, [serviceId])

  const save = useCallback(
    (view: Omit<SavedView, 'id' | 'createdAtMs' | 'lastUsedMs'>) => {
      const now = Date.now()
      const full: SavedView = {
        ...view,
        id: makeViewId(),
        createdAtMs: now,
        lastUsedMs: now,
      }
      upsertView(serviceId, full)
      emitChange(serviceId)
      return full
    },
    [serviceId]
  )

  const update = useCallback(
    (view: SavedView) => {
      upsertView(serviceId, view)
      emitChange(serviceId)
    },
    [serviceId]
  )

  const remove = useCallback(
    (viewId: string) => {
      deleteView(serviceId, viewId)
      emitChange(serviceId)
    },
    [serviceId]
  )

  const touch = useCallback(
    (viewId: string) => {
      touchView(serviceId, viewId)
      emitChange(serviceId)
    },
    [serviceId]
  )

  return { views, save, update, remove, touch }
}

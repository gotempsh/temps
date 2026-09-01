// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Per-service saved views for the Data Browser.
 *
 * A view captures a complete navigational + filter state snapshot so it can
 * be recalled via the command bar or a pinned item in the tree. Stored in
 * localStorage keyed by service id; no network, no PII.
 */

const STORAGE_KEY = 'temps:data-browser:views:v1'
const MAX_PER_SERVICE = 50

export interface SavedView {
  id: string
  name: string
  path: string
  entity?: string
  filter?: unknown
  sortField?: string
  sortOrder?: 'asc' | 'desc'
  pinned?: boolean
  createdAtMs: number
  lastUsedMs: number
}

type ViewStore = Record<string, SavedView[]>

function loadStore(): ViewStore {
  if (typeof window === 'undefined') return {}
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' ? (parsed as ViewStore) : {}
  } catch {
    return {}
  }
}

function saveStore(store: ViewStore): void {
  if (typeof window === 'undefined') return
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(store))
  } catch {
    /* quota exceeded or disabled — silent */
  }
}

export function listViews(serviceId: string): SavedView[] {
  const store = loadStore()
  return store[serviceId] ?? []
}

export function upsertView(serviceId: string, view: SavedView): SavedView[] {
  const store = loadStore()
  const existing = store[serviceId] ?? []
  const without = existing.filter((v) => v.id !== view.id)
  const next = [view, ...without].slice(0, MAX_PER_SERVICE)
  store[serviceId] = next
  saveStore(store)
  return next
}

export function deleteView(serviceId: string, viewId: string): SavedView[] {
  const store = loadStore()
  const existing = store[serviceId] ?? []
  const next = existing.filter((v) => v.id !== viewId)
  store[serviceId] = next
  saveStore(store)
  return next
}

export function touchView(serviceId: string, viewId: string): SavedView[] {
  const store = loadStore()
  const existing = store[serviceId] ?? []
  const next = existing.map((v) =>
    v.id === viewId ? { ...v, lastUsedMs: Date.now() } : v
  )
  store[serviceId] = next
  saveStore(store)
  return next
}

export function viewToUrlParams(view: SavedView): URLSearchParams {
  const p = new URLSearchParams()
  if (view.path) p.set('path', view.path)
  if (view.entity) p.set('entity', view.entity)
  if (view.sortField) {
    p.set('sort_by', view.sortField)
    p.set('sort_order', view.sortOrder ?? 'asc')
  }
  if (view.filter !== undefined) {
    try {
      p.set(
        'filter',
        typeof view.filter === 'string'
          ? view.filter
          : JSON.stringify(view.filter)
      )
    } catch {
      /* ignore */
    }
  }
  return p
}

export function makeViewId(): string {
  return `v_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`
}

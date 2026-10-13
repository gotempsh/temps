// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore } from 'react'

/** Where the workspace moves its side panel from a sheet to a docked aside. */
const WIDE_QUERY = '(min-width: 1280px)'

function subscribe(onStoreChange: () => void) {
  if (typeof window.matchMedia !== 'function') return () => {}
  const query = window.matchMedia(WIDE_QUERY)
  query.addEventListener('change', onStoreChange)
  return () => query.removeEventListener('change', onStoreChange)
}

function getSnapshot() {
  return typeof window.matchMedia === 'function'
    ? window.matchMedia(WIDE_QUERY).matches
    : true
}

/**
 * Whether the docked right panel (rather than the mobile sheet) is the one the
 * user sees. Both are rendered, one hidden by CSS, so anything that must exist
 * once — a terminal holds a socket and a PTY size — has to pick by this, not
 * by `hidden`.
 */
export function useWorkspaceWideLayout(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot, () => true)
}

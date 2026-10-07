// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shared state for the header operations tray.
 *
 * A tiny subscribable store (same pattern as `lib/project-tour.ts`) so any
 * component can open the tray or register a client-only entry without a
 * provider:
 *
 * - `openOperationsTray()` opens the popover in the header.
 * - `trackLocalOperation()` adds an entry for work that has no persisted
 *   record (container restarts). Local entries live in memory only — they are
 *   shown while the request is in flight and do not survive a refresh.
 * - `announceOperation()` refreshes the feed and shows a toast with a
 *   "View in operations" action.
 */
import { invalidateOperations } from '@/lib/operations'
import type { QueryClient } from '@tanstack/react-query'
import { useSyncExternalStore } from 'react'
import { toast } from 'sonner'

export interface LocalOperation {
  id: string
  title: string
  context: string | null
  /** Epoch milliseconds. */
  startedAt: number
}

interface TrayState {
  open: boolean
  local: readonly LocalOperation[]
}

let state: TrayState = { open: false, local: [] }
const listeners = new Set<() => void>()
let localSequence = 0

function setState(next: TrayState) {
  state = next
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function getOperationsTrayState(): TrayState {
  return state
}

export function setOperationsTrayOpen(open: boolean) {
  if (state.open === open) return
  setState({ ...state, open })
}

export function openOperationsTray() {
  setOperationsTrayOpen(true)
}

/**
 * Show a client-only entry until the returned function is called (on settle).
 */
export function trackLocalOperation(
  entry: Omit<LocalOperation, 'id' | 'startedAt'>
): () => void {
  localSequence += 1
  const id = `local:${localSequence}`
  const operation: LocalOperation = { ...entry, id, startedAt: Date.now() }
  setState({ ...state, local: [operation, ...state.local] })
  return () => {
    setState({
      ...state,
      local: state.local.filter((item) => item.id !== id),
    })
  }
}

export function useOperationsTrayOpen(): boolean {
  return useSyncExternalStore(subscribe, () => state.open)
}

export function useLocalOperations(): readonly LocalOperation[] {
  return useSyncExternalStore(subscribe, () => state.local)
}

/** Toast action that opens the tray. */
export const VIEW_IN_OPERATIONS_ACTION = {
  label: 'View in operations',
  onClick: () => openOperationsTray(),
}

/**
 * Refresh the operations feed and confirm with a toast that links to the
 * tray. Pass `queryClient: null` for client-only work with no feed entry.
 */
export function announceOperation(
  queryClient: QueryClient | null,
  message: string,
  options: { description?: string; id?: string | number } = {}
) {
  if (queryClient) void invalidateOperations(queryClient)
  toast.success(message, {
    id: options.id,
    description: options.description,
    action: VIEW_IN_OPERATIONS_ACTION,
  })
}

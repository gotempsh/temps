// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState } from 'react'

/**
 * Wording for a delete confirmation. Kept as a pure function so the copy that
 * names the target (and the pending label) can be asserted without a DOM.
 */
export interface ConfirmDeleteCopyInput {
  /** Noun for the thing being deleted, e.g. "alert rule". */
  resourceType: string
  /** Display name of the target. Quoted in the description when present. */
  resourceName?: string | null
  isPending?: boolean
  confirmLabel?: string
}

export interface ConfirmDeleteCopy {
  title: string
  description: string
  confirmLabel: string
}

export function confirmDeleteCopy({
  resourceType,
  resourceName,
  isPending = false,
  confirmLabel = 'Delete',
}: ConfirmDeleteCopyInput): ConfirmDeleteCopy {
  const name = resourceName?.trim()
  const subject = name ? `${resourceType} “${name}”` : `this ${resourceType}`
  return {
    title: name
      ? `Delete ${resourceType} “${name}”?`
      : `Delete ${resourceType}?`,
    description: `This permanently deletes ${subject}. This action cannot be undone.`,
    confirmLabel: isPending ? 'Deleting…' : confirmLabel,
  }
}

export interface ConfirmDeleteCallbacks {
  /** Runs when the delete succeeded; the hook closes the dialog itself. */
  onSuccess: () => void
}

export interface ConfirmDeleteController<T> {
  /** The item awaiting confirmation, or null when the dialog is closed. */
  target: T | null
  /** Whether the confirmation dialog should be open. */
  open: boolean
  /** Ask for confirmation. Sends nothing. */
  request: (target: T) => void
  /** Dismiss without deleting. */
  cancel: () => void
  /**
   * Run the delete for the pending target. A no-op when nothing is pending,
   * so a stray double click cannot send a request for a stale item.
   */
  confirm: () => void
}

/**
 * Two-step delete: `request(item)` opens the confirmation, `confirm()` is the
 * only path that calls `performDelete`. Pair with `ConfirmDeleteDialog`.
 *
 * `performDelete` should use `mutation.mutate(vars, callbacks)` (never
 * `mutateAsync` without a catch, which turns a failed request into an
 * unhandled rejection). The dialog stays open while the request is in flight
 * and after a failure, so the user can retry or cancel.
 */
export function useConfirmDelete<T>(
  performDelete: (target: T, callbacks: ConfirmDeleteCallbacks) => void
): ConfirmDeleteController<T> {
  const [target, setTarget] = useState<T | null>(null)

  const request = useCallback((next: T) => setTarget(next), [])
  const cancel = useCallback(() => setTarget(null), [])
  const confirm = () => {
    if (target === null) return
    performDelete(target, { onSuccess: () => setTarget(null) })
  }

  return { target, open: target !== null, request, cancel, confirm }
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { useContext } from 'react'
import { toast } from 'sonner'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { problemDetail } from '@/lib/api-problem'
import { internalAccessWriteBlock } from './internal-access'

/** Why the signed-in user cannot change internal access, or `null`. */
export function useInternalAccessWriteBlock(): string | null {
  // Read without `useAuth`, which throws outside its provider: a page
  // rendered without one simply lets the server decide.
  const auth = useContext(AuthContext)
  return internalAccessWriteBlock(auth?.user?.role)
}

/**
 * One row's Allow, with its toasts. It takes the request body, so the
 * one-click and "Limit…" paths share the pending and error state.
 */
export function useAllowGrant<Body>({
  allow,
  onChanged,
  allowed,
}: {
  allow: (body: Body) => Promise<unknown>
  onChanged: () => void
  /** The success toast. */
  allowed: string
}) {
  return useMutation({
    mutationFn: allow,
    onSuccess: () => {
      onChanged()
      toast.success(allowed)
    },
    onError: (error) => {
      toast.error(problemDetail(error, 'Failed to allow internal access'))
    },
  })
}

/**
 * A Remove mutation's toasts. The mutation itself stays in the row's module,
 * beside the `ConfirmDeleteDialog` that alone may fire it.
 */
export function removalFeedback(onChanged: () => void, removed: string) {
  return {
    onSuccess: () => {
      onChanged()
      toast.success(removed)
    },
    onError: (error: unknown) => {
      toast.error(problemDetail(error, 'Failed to remove internal access'))
    },
  }
}

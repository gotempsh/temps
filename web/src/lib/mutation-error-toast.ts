// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { toast } from 'sonner'

/**
 * What react-query hands a mutation's `onError` as its fourth argument
 * (`MutationFunctionContext`). The third argument is the `onMutate` result,
 * which is `undefined` for almost every mutation here, so a handler that reads
 * `meta` from it never finds the title the action was written with.
 */
export interface MutationErrorContext {
  meta?: Record<string, unknown>
  mutationKey?: readonly unknown[]
}

interface ProblemLike {
  title?: string | null
  detail?: string | null
}

/**
 * The title the mutation was authored with: `meta.errorTitle`, else a phrase
 * derived from its first `mutationKey` entry ("createProject" -> "Failed to
 * create project"), else `defaultTitle` (normally the server's own title).
 */
export function getErrorTitle(
  context: MutationErrorContext | undefined,
  defaultTitle?: string | null
): string | undefined {
  const authored = context?.meta?.errorTitle
  if (typeof authored === 'string' && authored) return authored

  const mutationKey = context?.mutationKey?.[0]
  if (typeof mutationKey === 'string' && mutationKey) {
    return `Failed to ${mutationKey.replace(/([A-Z])/g, ' $1').toLowerCase()}`
  }

  return defaultTitle || undefined
}

/**
 * Announce a failed mutation: the authored title when there is one, with the
 * server's reason underneath. Always shows something, so a refused action is
 * never silent.
 */
export function announceMutationFailure(
  problem: ProblemLike,
  context: MutationErrorContext | undefined
): void {
  const title =
    getErrorTitle(context, problem.title) ||
    problem.title ||
    'An error occurred'
  toast.error(
    title,
    problem.detail ? { description: problem.detail } : undefined
  )
}

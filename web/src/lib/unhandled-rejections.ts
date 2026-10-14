// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { extractProblemDetails } from '@/utils/errorHandling'
import { toast } from 'sonner'

/**
 * Failed API calls reject with the response body, a plain object, not an
 * `Error`. A handler that awaits one without a `catch` therefore ends as an
 * "uncaught exception: Object": no stack, no message, and nothing on screen.
 *
 * Most of those calls are mutations whose failure the query client has
 * already shown as a toast, so the rejection carries no news; for the rest
 * the user was told nothing. This module remembers which errors the user has
 * already been told about and reports the others, so a missed `catch` costs a
 * toast, not a silent failure.
 */

const reported = new WeakSet<object>()

/** Record that the user has been told about `error`. */
export function markErrorReported(error: unknown): void {
  if (error !== null && typeof error === 'object') reported.add(error)
}

export function wasErrorReported(error: unknown): boolean {
  return error !== null && typeof error === 'object' && reported.has(error)
}

/** What to tell the user about a rejection that nobody handled. */
export interface RejectionNotice {
  title: string
  description?: string
}

/**
 * The notice for a rejected value, or `undefined` when there is nothing to
 * add: the user was already told, or the value says nothing a person could
 * act on.
 */
export function rejectionNotice(reason: unknown): RejectionNotice | undefined {
  if (wasErrorReported(reason)) return undefined
  const problem = extractProblemDetails(reason)
  if (problem) {
    return {
      title: problem.title || 'Request failed',
      ...(problem.detail ? { description: problem.detail } : {}),
    }
  }
  if (reason instanceof Error && reason.message) {
    return { title: 'Something went wrong', description: reason.message }
  }
  return undefined
}

/**
 * For a `catch` that exists only so a failed submit does not escape as an
 * unhandled rejection: tell the user, unless the mutation already did.
 */
export function reportSubmitFailure(error: unknown): void {
  console.error('[console] submit failed', error)
  const notice = rejectionNotice(error)
  if (!notice) return
  markErrorReported(error)
  toast.error(notice.title, { description: notice.description })
}

/**
 * Report promise rejections nobody handled.
 *
 * Only the console's own API failures (response bodies) are intercepted: they
 * are expected outcomes with a message for the user. A real `Error` is a bug
 * and keeps the browser's default reporting so it stays visible in devtools
 * and error trackers.
 */
export function installUnhandledRejectionReporter(
  notify: (notice: RejectionNotice) => void,
  target: Pick<Window, 'addEventListener' | 'removeEventListener'> = window
): () => void {
  const onRejection = (event: PromiseRejectionEvent) => {
    const reason: unknown = event.reason
    if (reason instanceof Error) return
    if (reason === null || typeof reason !== 'object') return
    event.preventDefault()
    console.error('[console] unhandled API rejection', reason)
    const notice = rejectionNotice(reason)
    if (notice) {
      markErrorReported(reason)
      notify(notice)
    }
  }
  target.addEventListener('unhandledrejection', onRejection)
  return () => target.removeEventListener('unhandledrejection', onRejection)
}

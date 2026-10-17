// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isForbiddenRead, isVerifiedNotFound } from './read-failure'

/**
 * What the project page renders for the state of its project read.
 *
 * - `not-found`: the server answered 404, or there is no slug to look up.
 *   Only this says the project does not exist.
 * - `forbidden`: the server refused the caller (401/403), e.g. the project is
 *   granted to teams the user is not in. The project exists; the user lacks
 *   access, which a retry cannot fix, so the page says so instead.
 * - `failed`: the read failed for any other reason -- a 5xx, a dropped
 *   connection. The project may well exist, so the page offers a retry
 *   instead of claiming it is gone.
 * - `loading`: the first read is in flight.
 * - `ready`: the project is loaded.
 */
export type ProjectLoadState =
  'not-found' | 'forbidden' | 'failed' | 'loading' | 'ready'

export function projectLoadState({
  slug,
  error,
  isLoading,
  hasProject,
}: {
  slug: string | undefined
  error: unknown
  isLoading: boolean
  hasProject: boolean
}): ProjectLoadState {
  if (!slug) return 'not-found'
  if (error) {
    if (isVerifiedNotFound(error)) return 'not-found'
    return isForbiddenRead(error) ? 'forbidden' : 'failed'
  }
  if (isLoading) return 'loading'
  // Settled without an error but without a project: nothing to show.
  return hasProject ? 'ready' : 'not-found'
}

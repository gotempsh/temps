// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isVerifiedNotFound } from './read-failure'

/**
 * What the project page renders for the state of its project read.
 *
 * - `not-found`: the server answered 404, or there is no slug to look up.
 *   Only this says the project does not exist.
 * - `failed`: the read failed for any other reason -- a 5xx, a 403, a
 *   dropped connection. The project may well exist, so the page offers a
 *   retry instead of claiming it is gone.
 * - `loading`: the first read is in flight.
 * - `ready`: the project is loaded.
 */
export type ProjectLoadState = 'not-found' | 'failed' | 'loading' | 'ready'

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
  if (error) return isVerifiedNotFound(error) ? 'not-found' : 'failed'
  if (isLoading) return 'loading'
  // Settled without an error but without a project: nothing to show.
  return hasProject ? 'ready' : 'not-found'
}

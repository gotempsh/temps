// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectSource } from './NewProjectShell'

export interface GitConnectionsRead {
  /** Last successfully read connection list; `undefined` before any. */
  connections: { connections: readonly unknown[] } | undefined
  /** Whether the most recent read failed. */
  isError: boolean
}

/**
 * Whether nothing trustworthy is known about the user's Git connections.
 *
 * True while the first read is pending, and when the latest read failed with
 * no connection cached. A cached *empty* list does not count as known: the
 * failed refresh is the current answer, so "No Git provider connected" would
 * be a claim the console cannot make.
 */
export function gitConnectionsUnknown({
  connections,
  isError,
}: GitConnectionsRead): boolean {
  return (
    connections === undefined ||
    (isError && connections.connections.length === 0)
  )
}

/**
 * Where `/projects/new` lands when no `?source=` was chosen.
 *
 * With a Git connection, the provider's repository list; with a verified
 * empty connection list, the template gallery. While connections are unknown
 * nothing is decided: landing on templates then would tell a user whose read
 * failed that they have no Git provider.
 */
export function newProjectLandingSource(
  read: GitConnectionsRead
): ProjectSource | null {
  if (gitConnectionsUnknown(read) || !read.connections) return null
  return read.connections.connections.length > 0 ? 'browse' : 'templates'
}

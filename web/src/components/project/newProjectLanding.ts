// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectSource } from './NewProjectShell'

/**
 * Where `/projects/new` lands when no `?source=` was chosen.
 *
 * With a Git connection, the provider's repository list; with a verified
 * empty connection list, the template gallery. `undefined` connections (still
 * loading, or the read failed) decide nothing: landing on templates then
 * would tell a user whose read failed that they have no Git provider.
 */
export function newProjectLandingSource(
  connections: { connections: readonly unknown[] } | undefined
): ProjectSource | null {
  if (!connections) return null
  return connections.connections.length > 0 ? 'browse' : 'templates'
}

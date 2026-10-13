// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useContext } from 'react'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { isInstanceAdmin } from '@/lib/instance-admin'

/**
 * Whether the signed-in user administers the instance. Gate admin-only reads
 * on this (`enabled: isAdmin`) so other roles never send requests the server
 * is certain to refuse.
 *
 * Reads the auth context without requiring it, so a hook used by a component
 * rendered outside the provider (a test, an embedded preview) treats the
 * viewer as a non-admin instead of throwing.
 */
export function useIsInstanceAdmin(): boolean {
  const auth = useContext(AuthContext)
  return isInstanceAdmin(auth?.user?.role)
}

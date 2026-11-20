// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useContext } from 'react'
import { AuthContext } from '@/contexts/AuthContext-shared'

/**
 * The signed-in user's role (`admin`, `user`, ...), or `undefined` while the
 * session is loading or when rendered outside the auth provider.
 *
 * Like `useIsInstanceAdmin`, this reads the context without requiring it so a
 * component that only wants to name the role in a message can render in a test
 * or an embedded preview.
 */
export function useCurrentUserRole(): string | undefined {
  return useContext(AuthContext)?.user?.role
}

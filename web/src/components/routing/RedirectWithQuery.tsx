// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, useLocation } from 'react-router'
import { redirectTarget } from '@/lib/redirect-target'

/** `<Navigate replace>` that keeps the current query string and hash. */
export function RedirectWithQuery({ to }: { to: string }) {
  const { search, hash } = useLocation()
  return <Navigate to={redirectTarget(to, search, hash)} replace />
}

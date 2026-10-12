// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, useLocation, useParams } from 'react-router'
import { buildRedirectTarget } from '@/lib/redirect-target'

/**
 * Replaces a legacy route with its new home, carrying the query string and
 * hash over. Use instead of `<Navigate to="..." replace />` for any route kept
 * only so old links keep working.
 */
export function RedirectWithSearch({ to }: { to: string }) {
  const { search, hash } = useLocation()
  const params = useParams()
  return (
    <Navigate to={buildRedirectTarget({ to, search, hash, params })} replace />
  )
}

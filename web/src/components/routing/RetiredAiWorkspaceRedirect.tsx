// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, useLocation } from 'react-router'
import { retiredAiWorkspaceTarget } from '@/lib/retired-ai-workspace'

/** Sends a bookmarked `/ai-first` link to the workspace it pointed at. */
export function RetiredAiWorkspaceRedirect() {
  const { search } = useLocation()
  return <Navigate to={retiredAiWorkspaceTarget(search)} replace />
}

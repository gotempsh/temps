// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useState } from 'react'
import type { AccessSessionResponse } from '@/api/client'
export function sessionHref(
  session: Pick<AccessSessionResponse, 'project_id' | 'vault_id' | 'id'>
) {
  return `/vault/projects/${session.project_id}/vaults/${encodeURIComponent(session.vault_id)}/sessions/${encodeURIComponent(session.id)}`
}
export function useSessionClock() {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [])
  return now
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useSelfUpdateCapability } from '@/hooks/useSelfUpdate'
import { releaseTag } from '@/lib/worker-join-script'

/** How long a join command waits for this server's version. */
export const PLANE_RELEASE_WAIT_MS = 10_000

/**
 * The release this control plane runs, for pinning a new worker's install
 * to it, and whether it is still being read. The wait is bounded: past
 * {@link PLANE_RELEASE_WAIT_MS}, or when the read fails, `tag` is `null`
 * and callers show the install unpinned with a warning.
 */
export function usePlaneRelease(): { tag: string | null; pending: boolean } {
  const { data, isLoading } = useSelfUpdateCapability()
  const [waitOver, setWaitOver] = useState(false)
  useEffect(() => {
    if (!isLoading) return
    const timer = setTimeout(() => setWaitOver(true), PLANE_RELEASE_WAIT_MS)
    return () => clearTimeout(timer)
  }, [isLoading])
  return {
    tag: releaseTag(data?.current_version),
    pending: isLoading && !waitOver,
  }
}

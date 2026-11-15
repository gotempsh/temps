// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useSelfUpdateCapability } from '@/hooks/useSelfUpdate'
import { type WorkerInstall, workerInstallFrom } from '@/lib/worker-join-script'

/** How long a join command waits for this server's version. */
export const PLANE_RELEASE_WAIT_MS = 10_000

/**
 * What a new worker installs to match this control plane (the release it
 * runs, or the newest nightly build when it runs a build between releases),
 * and whether it is still being read. The wait is bounded: past
 * {@link PLANE_RELEASE_WAIT_MS}, or when the read fails, `install` is the
 * newest stable release and callers say it may not match.
 */
export function usePlaneRelease(): {
  install: WorkerInstall
  pending: boolean
} {
  const { data, isLoading } = useSelfUpdateCapability()
  const [waitOver, setWaitOver] = useState(false)
  useEffect(() => {
    if (!isLoading) return
    const timer = setTimeout(() => setWaitOver(true), PLANE_RELEASE_WAIT_MS)
    return () => clearTimeout(timer)
  }, [isLoading])
  return {
    install: workerInstallFrom(data),
    pending: isLoading && !waitOver,
  }
}

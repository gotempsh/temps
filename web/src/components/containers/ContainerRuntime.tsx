// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SandboxBackendLogo } from '@/components/sandboxes/SandboxBackendLogo'
import { cn } from '@/lib/utils'
import { containerRuntime, containerRuntimeLabel } from './container-runtime'

/** The logo and name of what runs a container (Docker, a Firecracker microVM). */
export function ContainerRuntimeBadge({
  container,
  className,
}: {
  container: { runtime?: string | null }
  className?: string
}) {
  const runtime = containerRuntime(container)
  return (
    <span className={cn('inline-flex items-center gap-1.5', className)}>
      <SandboxBackendLogo backend={runtime} />
      <span>{containerRuntimeLabel(runtime)}</span>
    </span>
  )
}

/** The runtime's logo alone, sized for a row's leading tile. */
export function ContainerRuntimeLogo({
  container,
  className,
}: {
  container: { runtime?: string | null }
  className?: string
}) {
  const runtime = containerRuntime(container)
  return (
    <span title={containerRuntimeLabel(runtime)} className="inline-flex">
      <SandboxBackendLogo
        backend={runtime}
        className={cn('size-5', className)}
      />
    </span>
  )
}

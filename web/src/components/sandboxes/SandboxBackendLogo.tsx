// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import { Box } from 'lucide-react'
import { sandboxBackendLogo } from './sandbox-backend-logo'

/**
 * The logo of the isolation backend a sandbox runs on. Unknown or unrecorded
 * backends get a neutral box so the slot never collapses.
 */
export function SandboxBackendLogo({
  backend,
  className,
}: {
  backend?: string | null
  className?: string
}) {
  const logo = sandboxBackendLogo(backend)
  if (!logo) {
    return <Box className={cn('size-3.5 shrink-0', className)} aria-hidden />
  }
  if (logo.markOnly) {
    // The upstream file is the mark plus a wordmark. Clip it to the square at
    // its left edge, nudged so the mark sits in the middle, instead of editing
    // the file.
    return (
      <span
        className={cn(
          'inline-block size-3.5 shrink-0 overflow-hidden',
          className
        )}
        role="img"
        aria-label={logo.label}
      >
        <img
          src={logo.src}
          alt=""
          className="size-full max-w-none object-cover"
          style={{ objectPosition: '-2% 50%' }}
        />
      </span>
    )
  }
  return (
    <img
      src={logo.src}
      alt={logo.label}
      className={cn('size-3.5 shrink-0 object-contain', className)}
    />
  )
}

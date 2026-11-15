// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { SandboxBackendLogo } from '@/components/sandboxes/SandboxBackendLogo'
import { sandboxBackendLogo } from '@/components/sandboxes/sandbox-backend-logo'
import { cn } from '@/lib/utils'
import { SquareTerminal } from 'lucide-react'

/**
 * Marks a chat that has a sandbox, with the logo of the backend it runs on.
 * Small enough for the conversation list's second line.
 */
export function ConversationSandboxChip({
  name,
  backend,
  className,
}: {
  name: string
  backend?: string | null
  className?: string
}) {
  const backendLabel = sandboxBackendLogo(backend)?.label
  return (
    <span
      className={cn(
        'inline-flex shrink-0 items-center gap-1 rounded border bg-background px-1 py-px text-muted-foreground',
        className
      )}
      title={`Sandbox ${name}${backendLabel ? ` · ${backendLabel}` : ''}`}
    >
      <SquareTerminal className="size-3" aria-hidden />
      <SandboxBackendLogo backend={backend} />
      <span className="sr-only">{`Sandbox ${name}`}</span>
    </span>
  )
}

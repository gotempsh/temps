// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Avatar, AvatarFallback } from '@temps-sdk/ui'
import { cn } from './lib/cn'

export interface ProjectAvatarProps {
  name: string
  className?: string
  fallbackClassName?: string
}

/**
 * Deterministic project identity for surfaces that do not have deployment
 * media — pickers, ledger rows, record headers. The console has no
 * project-favicon endpoint, so this deliberately avoids issuing a guaranteed
 * 404 request; promoted from `web/src/components/project/ProjectAvatar.tsx`
 * (unchanged behavior — that file now re-exports this).
 */
export function ProjectAvatar({ name, className, fallbackClassName }: ProjectAvatarProps) {
  return (
    <Avatar className={className}>
      <AvatarFallback className={cn('font-medium', fallbackClassName)}>
        {name.slice(0, 1).toUpperCase()}
      </AvatarFallback>
    </Avatar>
  )
}

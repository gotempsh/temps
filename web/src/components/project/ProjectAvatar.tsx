// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Avatar, AvatarFallback } from '@/components/ui/avatar'
import { cn } from '@/lib/utils'

interface ProjectAvatarProps {
  name: string
  className?: string
  fallbackClassName?: string
}

/**
 * Deterministic project identity for surfaces that do not have deployment
 * media. The console has no project-favicon endpoint, so this deliberately
 * avoids issuing a guaranteed 404 request.
 */
export function ProjectAvatar({
  name,
  className,
  fallbackClassName,
}: ProjectAvatarProps) {
  return (
    <Avatar className={className}>
      <AvatarFallback className={cn('font-medium', fallbackClassName)}>
        {name.slice(0, 1).toUpperCase()}
      </AvatarFallback>
    </Avatar>
  )
}

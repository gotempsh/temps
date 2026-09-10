// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RepositoryResponse } from '@/api/client'
import { repositoryAvatarUrl } from '@/lib/repository-avatar'
import { cn } from '@/lib/utils'
import { useState } from 'react'

interface RepositoryAvatarProps {
  repository: RepositoryResponse
  providerType?: string | null
  className?: string
}

export function RepositoryAvatar({
  repository,
  providerType,
  className,
}: RepositoryAvatarProps) {
  const avatarUrl = repositoryAvatarUrl(repository, providerType)
  const [failedUrl, setFailedUrl] = useState<string | null>(null)

  if (avatarUrl && failedUrl !== avatarUrl) {
    return (
      <img
        src={avatarUrl}
        alt=""
        referrerPolicy="no-referrer"
        className={cn(
          'size-9 shrink-0 rounded-md object-cover outline-1 -outline-offset-1 outline-black/10 dark:outline-white/10',
          className
        )}
        onError={() => setFailedUrl(avatarUrl)}
      />
    )
  }

  return (
    <span
      aria-hidden="true"
      className={cn(
        'flex size-9 shrink-0 items-center justify-center rounded-md bg-muted font-mono text-sm font-medium text-muted-foreground outline-1 -outline-offset-1 outline-black/5 dark:outline-white/10',
        className
      )}
    >
      {repository.name.trim().charAt(0).toLowerCase() || '?'}
    </span>
  )
}

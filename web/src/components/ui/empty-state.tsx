// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { LucideIcon } from 'lucide-react'
import { ReactNode } from 'react'
import { cn } from '@/lib/utils'

interface EmptyStateProps {
  icon: LucideIcon
  title: string
  description: ReactNode | string
  action?: ReactNode
  size?: 'default' | 'compact'
}

export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  size = 'default',
}: EmptyStateProps) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center rounded-lg text-center animate-in fade-in-50',
        size === 'compact' ? 'min-h-60 gap-3 p-6' : 'min-h-[400px] gap-4 p-8'
      )}
    >
      <div
        className={cn(
          'flex items-center justify-center rounded-full bg-muted',
          size === 'compact' ? 'size-14' : 'size-20'
        )}
      >
        <Icon
          className={cn(
            'text-muted-foreground',
            size === 'compact' ? 'size-7' : 'size-10'
          )}
        />
      </div>
      <div className="max-w-md space-y-2">
        <h3
          className={cn(
            'font-semibold',
            size === 'compact' ? 'text-base' : 'text-lg'
          )}
        >
          {title}
        </h3>
        {typeof description === 'string' ? (
          <p className="text-sm text-muted-foreground">{description}</p>
        ) : (
          description
        )}
      </div>
      {action}
    </div>
  )
}

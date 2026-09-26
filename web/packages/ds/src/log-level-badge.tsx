// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@temps-sdk/ui'
import { cn } from './lib/cn'

/** Shared severity labels: routine output stays neutral; warnings and errors stand out. */
export function LogLevelBadge({ level }: { level: string }) {
  return (
    <Badge
      variant="outline"
      className={cn(
        'h-4.5 shrink-0 rounded-sm border-transparent px-1.5 py-0 font-mono text-xs font-medium leading-none',
        level === 'ERROR' || level === 'FATAL'
          ? 'bg-destructive/10 text-destructive'
          : level === 'WARN' || level === 'WARNING'
            ? 'bg-warning text-warning-foreground'
            : 'bg-transparent text-muted-foreground'
      )}
    >
      {level}
    </Badge>
  )
}

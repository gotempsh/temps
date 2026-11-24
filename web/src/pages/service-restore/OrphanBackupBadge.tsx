// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { badgeVariants } from '@/components/ui/badge'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'

export function OrphanBackupBadge({ label = 'orphan' }: { label?: string }) {
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <span
            tabIndex={0}
            className={badgeVariants({
              variant: 'outline',
              className: 'cursor-help text-xs',
            })}
          >
            {label}
          </span>
        </TooltipTrigger>
        <TooltipContent className="max-w-64">
          This backup was found in S3 and has no backup record on this Temps
          server. Another server may have created it. To restore it, open the
          destination database’s Restore page and search this S3 source.
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}

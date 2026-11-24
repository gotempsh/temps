// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { badgeVariants } from '@/components/ui/badge'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'

/**
 * Marks a backup found in S3 that this Temps server has no record of. The one
 * name for such a backup on every page: not "orphan", which implies the
 * server that made it is gone, when it may still be running.
 */
export function FoundInS3Badge() {
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
            Found in S3
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

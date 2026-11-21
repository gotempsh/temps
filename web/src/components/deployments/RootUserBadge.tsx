// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ShieldAlert } from 'lucide-react'
import { badgeVariants } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { cn } from '@/lib/utils'
import {
  describeImageUser,
  NON_ROOT_USER_DOCS_URL,
  NON_ROOT_USER_SNIPPET,
} from './root-user'

/**
 * A compact warning for a deployment whose image runs as root (uid 0), with
 * the fix one click away. Renders nothing for any other deployment, including
 * one that was never checked.
 */
export function RootUserBadge({
  runsAsRoot,
  imageUser,
  className,
}: {
  runsAsRoot?: boolean | null
  imageUser?: string | null
  className?: string
}) {
  if (!runsAsRoot) return null
  return (
    <Popover>
      <PopoverTrigger
        className={cn(
          badgeVariants({ variant: 'warning-subtle' }),
          'shrink-0 cursor-pointer gap-1 py-1 pl-1.5 pr-2.5',
          className
        )}
        aria-label="Runs as root: how to run as a non-root user"
      >
        <ShieldAlert className="h-3.5 w-3.5" aria-hidden="true" />
        Runs as root
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 space-y-3 text-sm">
        <div className="space-y-1">
          <p className="font-medium">This image runs as root (uid 0)</p>
          <p className="text-muted-foreground">
            {describeImageUser(imageUser)} Add a non-root user to the Dockerfile
            and redeploy.
          </p>
        </div>
        <div className="flex items-start justify-between gap-2 rounded-md border bg-muted/30 p-2">
          <pre className="min-w-0 whitespace-pre-wrap font-mono text-xs [overflow-wrap:anywhere]">
            {NON_ROOT_USER_SNIPPET}
          </pre>
          <CopyButton
            className="size-7 shrink-0 rounded-md"
            label="Copy the non-root USER lines"
            minimal
            value={NON_ROOT_USER_SNIPPET}
          />
        </div>
        <a
          href={NON_ROOT_USER_DOCS_URL}
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:text-primary/80"
        >
          How to run as a non-root user
        </a>
      </PopoverContent>
    </Popover>
  )
}

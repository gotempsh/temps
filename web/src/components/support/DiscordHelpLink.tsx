// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ExternalLink } from 'lucide-react'
import { DISCORD_INVITE_URL } from '@/lib/support'
import { cn } from '@/lib/utils'

export function DiscordHelpLink({ className }: { className?: string }) {
  return (
    <a
      href={DISCORD_INVITE_URL}
      target="_blank"
      rel="noopener noreferrer"
      aria-label="Ask on Discord (opens in a new tab)"
      className={cn(
        'inline-flex items-center gap-1 rounded-sm font-medium text-foreground underline underline-offset-4 hover:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
        className
      )}
    >
      Ask on Discord
      <ExternalLink aria-hidden="true" className="size-3 shrink-0" />
    </a>
  )
}

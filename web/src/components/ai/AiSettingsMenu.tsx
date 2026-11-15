// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { BarChart3, Box, KeyRound, Settings } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { Link } from 'react-router'

interface SettingsLink {
  to: string
  label: string
  detail: string
  Icon: LucideIcon
}

/** Where the assistant's settings live, by what the user wants to change. */
const AI_SETTINGS_LINKS: readonly SettingsLink[] = [
  {
    to: '/ai-gateway',
    label: 'AI providers & API keys',
    detail: 'Add or replace keys, choose models',
    Icon: KeyRound,
  },
  {
    to: '/agent-sandbox',
    label: 'Sandboxes',
    detail: 'Where the assistant builds and runs code',
    Icon: Box,
  },
  {
    to: '/ai-gateway/usage',
    label: 'Usage & spend',
    detail: 'Requests, tokens and cost by model',
    Icon: BarChart3,
  },
]

/** The cog in the assistant's header: a way into its settings from the chat. */
export function AiSettingsMenu() {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          title="AI settings"
          aria-label="AI settings"
        >
          <Settings className="h-4 w-4" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-72">
        <DropdownMenuLabel>AI settings</DropdownMenuLabel>
        <DropdownMenuSeparator />
        {AI_SETTINGS_LINKS.map(({ to, label, detail, Icon }) => (
          <DropdownMenuItem key={to} asChild>
            <Link to={to} className="flex items-start gap-2.5">
              <Icon
                className="mt-0.5 size-4 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              <span className="min-w-0">
                <span className="block text-sm">{label}</span>
                <span className="block text-xs text-muted-foreground">
                  {detail}
                </span>
              </span>
            </Link>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { BookOpen, HelpCircle, type LucideIcon } from 'lucide-react'

export const DISCORD_INVITE_URL = 'https://discord.com/invite/ZunrteG6JX'

/** One page listing what a self-hosted install supports; `install.sh` prints it too. */
export const CAPABILITIES_DOCS_URL = 'https://temps.sh/docs/capabilities'

export interface SidebarHelpLink {
  id: string
  label: string
  href: string
  icon: LucideIcon
}

/** External links in the sidebar footer, in display order. */
export const SIDEBAR_HELP_LINKS: readonly SidebarHelpLink[] = [
  {
    id: 'capabilities',
    label: 'What Temps supports',
    href: CAPABILITIES_DOCS_URL,
    icon: BookOpen,
  },
  {
    id: 'discord',
    label: 'Need help? Join Discord',
    href: DISCORD_INVITE_URL,
    icon: HelpCircle,
  },
]

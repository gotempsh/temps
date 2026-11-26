// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Sparkles } from 'lucide-react'
import { Link, useLocation } from 'react-router'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'
import {
  SidebarGroup,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from '@/components/ui/sidebar'
import { ASSISTANT_URL, assistantHref } from '@/lib/assistant-entry'
import { ASSISTANT_AREA } from '@/lib/server-navigation'
import { cn } from '@/lib/utils'

/**
 * The assistant's entry, at the top of the sidebar on every page: it opens
 * the chat the user last had open, or a new one, inside the same console
 * shell as any other page.
 */
export function AskEntry() {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  // Read again after every navigation: the last chat changes on its page.
  const shown = useLocation().pathname === ASSISTANT_URL
  return (
    <SidebarGroup className="pb-0">
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton
            asChild
            tooltip={compact ? 'Ask' : undefined}
            className={cn(
              compact ? 'justify-center' : 'justify-start',
              shown && 'bg-sidebar-accent text-sidebar-accent-foreground'
            )}
          >
            <Link
              to={assistantHref()}
              aria-current={shown ? 'page' : undefined}
            >
              <Sparkles />
              {!compact && <span>Ask</span>}
              {!compact && (
                <FeatureMaturityBadge
                  featureKey={ASSISTANT_AREA.featureKey}
                  compact
                />
              )}
            </Link>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarGroup>
  )
}

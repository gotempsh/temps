// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ChevronsUpDown } from 'lucide-react'
import { useContext, type ComponentType } from 'react'
import { Button } from '@/components/ui/button'
import { SidebarContext } from '@/components/ui/sidebar-shared'

/**
 * On a phone, where the sidebar is a drawer: names the view shown and opens
 * the drawer on the entity's views, so they are chosen in the same place as
 * on a desktop and the way there is on the page, not only behind the menu
 * button. Renders nothing while the sidebar is on screen.
 */
export function ViewMenuButton({
  title,
  icon: Icon,
}: {
  /** The view shown, such as "Settings". */
  title: string
  icon: ComponentType<{ className?: string }>
}) {
  // A page drawn outside the console's frame has no sidebar to open.
  const sidebar = useContext(SidebarContext)
  if (!sidebar?.isMobile) return null
  return (
    <Button
      type="button"
      variant="outline"
      aria-label={`${title}. Show all views`}
      title="Show all views"
      onClick={() => sidebar.setOpenMobile(true)}
      className="mb-3 h-10 w-full justify-start gap-2 px-3"
    >
      <Icon
        className="size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
      <span className="min-w-0 flex-1 truncate text-left">{title}</span>
      <ChevronsUpDown
        className="size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
    </Button>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ComponentProps } from 'react'
import { GripVertical } from 'lucide-react'
import { Group, Panel, Separator } from 'react-resizable-panels'
import { cn } from '@/lib/utils'

/**
 * shadcn-style wrapper over react-resizable-panels.
 *
 * Note this targets the v4 API (`Group` / `Panel` / `Separator`), not the
 * `PanelGroup` / `PanelResizeHandle` names used by the published shadcn
 * snippet — those were renamed in v4, so copy/pasting that snippet fails.
 * Sizes follow the v4 convention: numbers are pixels, strings are percentages.
 */
function ResizablePanelGroup({
  className,
  ...props
}: ComponentProps<typeof Group>) {
  return (
    <Group
      className={cn(
        'flex h-full w-full data-[orientation=vertical]:flex-col',
        className
      )}
      {...props}
    />
  )
}

const ResizablePanel = Panel

/**
 * `withHandle` draws the visible grip. Without it the separator is still
 * draggable, just invisible until hover — fine between two panes that already
 * have a border, but a grip is clearer when they don't.
 */
function ResizableHandle({
  withHandle,
  className,
  ...props
}: ComponentProps<typeof Separator> & { withHandle?: boolean }) {
  return (
    <Separator
      className={cn(
        'relative flex w-px items-center justify-center bg-border',
        'after:absolute after:inset-y-0 after:left-1/2 after:w-1 after:-translate-x-1/2',
        'hover:bg-primary/40 data-[separator]:transition-colors',
        'focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring',
        'data-[orientation=vertical]:h-px data-[orientation=vertical]:w-full',
        'data-[orientation=vertical]:after:left-0 data-[orientation=vertical]:after:h-1',
        'data-[orientation=vertical]:after:w-full data-[orientation=vertical]:after:-translate-y-1/2',
        'data-[orientation=vertical]:after:translate-x-0',
        className
      )}
      {...props}
    >
      {withHandle && (
        <div className="z-10 flex h-8 w-3 items-center justify-center rounded-sm border bg-border">
          <GripVertical className="h-2.5 w-2.5" />
        </div>
      )}
    </Separator>
  )
}

export { ResizablePanelGroup, ResizablePanel, ResizableHandle }

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import * as TabsPrimitive from '@radix-ui/react-tabs'

import { cn } from '@/lib/utils'

const Tabs = TabsPrimitive.Root

/**
 * `pill` is shadcn's stock segmented-control look (bg-muted pill, active tab
 * gets a background + shadow) — it's the default so every existing consumer
 * is unaffected. The other three are structurally different treatments, not
 * just recolors, so they live here as variants rather than one-off forks:
 * pick per surface, but don't invent a fifth ad hoc style elsewhere.
 */
type TabsVariant = 'pill' | 'underline' | 'editor' | 'shortcut'

const TabsVariantContext = React.createContext<TabsVariant>('pill')

const LIST_CLASSES: Record<TabsVariant, string> = {
  pill: 'inline-flex h-10 items-center justify-center rounded-md bg-muted p-1 text-muted-foreground',
  underline: 'inline-flex items-center gap-6 border-b',
  editor: 'inline-flex items-end gap-0 border-b',
  shortcut: 'inline-flex items-center gap-1',
}

const TRIGGER_CLASSES: Record<TabsVariant, string> = {
  pill: 'inline-flex items-center justify-center whitespace-nowrap rounded-sm px-3 py-1.5 text-sm font-medium ring-offset-background transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm',
  underline:
    '-mb-px inline-flex items-center whitespace-nowrap border-b-2 border-transparent pb-3 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50 data-[state=active]:border-foreground data-[state=active]:text-foreground',
  editor:
    '-mb-px inline-flex items-center whitespace-nowrap rounded-t-md border border-transparent border-b-border px-3 py-2 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50 data-[state=active]:border-border data-[state=active]:border-b-background data-[state=active]:bg-background data-[state=active]:text-foreground',
  shortcut:
    'inline-flex items-center gap-2 whitespace-nowrap rounded-md px-2.5 py-1.5 text-sm font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50 data-[state=active]:bg-foreground data-[state=active]:text-background',
}

interface TabsListProps
  extends React.ComponentPropsWithoutRef<typeof TabsPrimitive.List> {
  variant?: TabsVariant
}

const TabsList = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.List>,
  TabsListProps
>(({ className, variant = 'pill', ...props }, ref) => (
  <TabsVariantContext.Provider value={variant}>
    <TabsPrimitive.List
      ref={ref}
      className={cn(LIST_CLASSES[variant], className)}
      {...props}
    />
  </TabsVariantContext.Provider>
))
TabsList.displayName = TabsPrimitive.List.displayName

const TabsTrigger = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>
>(({ className, ...props }, ref) => {
  const variant = React.useContext(TabsVariantContext)
  return (
    <TabsPrimitive.Trigger
      ref={ref}
      className={cn(TRIGGER_CLASSES[variant], className)}
      {...props}
    />
  )
})
TabsTrigger.displayName = TabsPrimitive.Trigger.displayName

const TabsContent = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Content
    ref={ref}
    className={cn(
      'mt-2 ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
      className
    )}
    {...props}
  />
))
TabsContent.displayName = TabsPrimitive.Content.displayName

export { Tabs, TabsList, TabsTrigger, TabsContent }
export type { TabsVariant }

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Database, Plus, Table as TableIcon, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

export interface BrowserTab {
  id: string
  path: string
  entity?: string
  sortField?: string
  sortOrder?: 'asc' | 'desc'
  filter?: unknown
  page?: number
}

interface DataBrowserTabsProps {
  tabs: BrowserTab[]
  activeTabId: string
  onActivate: (id: string) => void
  onClose: (id: string) => void
  onNewTab: () => void
}

function tabLabel(tab: BrowserTab): string {
  if (tab.entity) return tab.entity
  if (tab.path) {
    const segs = tab.path.split('/')
    return segs[segs.length - 1] || tab.path
  }
  return 'New'
}

function tabSubtitle(tab: BrowserTab): string {
  if (tab.entity && tab.path) return tab.path
  return ''
}

export function DataBrowserTabs({
  tabs,
  activeTabId,
  onActivate,
  onClose,
  onNewTab,
}: DataBrowserTabsProps) {
  if (tabs.length === 0) return null

  return (
    <div className="flex items-stretch gap-0.5 border-b bg-muted/20 overflow-x-auto scrollbar-thin">
      {tabs.map((tab) => {
        const isActive = tab.id === activeTabId
        const Icon = tab.entity ? TableIcon : Database
        return (
          <div
            key={tab.id}
            role="tab"
            aria-selected={isActive}
            tabIndex={0}
            onClick={() => onActivate(tab.id)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                onActivate(tab.id)
              }
            }}
            onAuxClick={(e) => {
              if (e.button === 1) {
                e.preventDefault()
                onClose(tab.id)
              }
            }}
            className={cn(
              'group flex items-center gap-2 px-3 py-1.5 text-xs border-r cursor-pointer select-none min-w-[120px] max-w-[240px] outline-none',
              'focus-visible:ring-2 focus-visible:ring-ring',
              isActive
                ? 'bg-background border-b-transparent -mb-px text-foreground'
                : 'text-muted-foreground hover:text-foreground hover:bg-background/60'
            )}
            title={tabSubtitle(tab) || tabLabel(tab)}
          >
            <Icon className="h-3.5 w-3.5 flex-shrink-0" />
            <div className="flex-1 min-w-0 flex flex-col">
              <span className="truncate font-medium">{tabLabel(tab)}</span>
              {tabSubtitle(tab) && (
                <span className="truncate text-[10px] text-muted-foreground/70">
                  {tabSubtitle(tab)}
                </span>
              )}
            </div>
            {tabs.length > 1 && (
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation()
                  onClose(tab.id)
                }}
                className={cn(
                  'flex-shrink-0 rounded-sm p-0.5 transition-opacity',
                  isActive
                    ? 'opacity-60 hover:opacity-100 hover:bg-muted'
                    : 'opacity-0 group-hover:opacity-60 hover:opacity-100 hover:bg-muted'
                )}
                aria-label="Close tab"
              >
                <X className="h-3 w-3" />
              </button>
            )}
          </div>
        )
      })}
      <Button
        type="button"
        variant="ghost"
        size="sm"
        onClick={onNewTab}
        className="h-auto px-2 text-muted-foreground hover:text-foreground rounded-none"
        aria-label="New tab"
      >
        <Plus className="h-3.5 w-3.5" />
      </Button>
    </div>
  )
}

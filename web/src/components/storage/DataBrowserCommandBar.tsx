// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState } from 'react'
import Fuse from 'fuse.js'
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from '@/components/ui/command'
import { Badge } from '@/components/ui/badge'
import {
  Bookmark,
  Clock,
  Database,
  ExternalLink,
  History,
  Table as TableIcon,
  Terminal,
} from 'lucide-react'
import { useFrecency } from '@/hooks/useFrecency'
import type { SavedView } from '@/lib/data-browser-views'
import { cn } from '@/lib/utils'

export interface CommandTarget {
  id: string
  kind: 'container' | 'entity'
  name: string
  path: string
  entity?: string
  /** e.g. "database", "schema", "table", "bucket" */
  label?: string
}

interface DataBrowserCommandBarProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  targets: CommandTarget[]
  views: SavedView[]
  /** Called when user picks a tree target. */
  onJump: (target: CommandTarget) => void
  /** Called when user picks a saved view. */
  onOpenView: (view: SavedView) => void
  /** Called when user picks the "run raw filter on current entity" action. */
  onRunRawQuery?: (raw: string) => void
  /** The current entity name, for contextual "run query" hint. */
  currentEntity?: string
  /** Whether the current entity supports SQL-style filter. */
  supportsSql?: boolean
}

function frecencyKey(t: CommandTarget): string {
  return `target:${t.id}`
}

function viewKey(v: SavedView): string {
  return `view:${v.id}`
}

export function DataBrowserCommandBar({
  open,
  onOpenChange,
  targets,
  views,
  onJump,
  onOpenView,
  onRunRawQuery,
  currentEntity,
  supportsSql,
}: DataBrowserCommandBarProps) {
  const [input, setInput] = useState('')
  const { record, blend, store } = useFrecency()

  const fuse = useMemo(
    () =>
      new Fuse(targets, {
        keys: [
          { name: 'name', weight: 0.7 },
          { name: 'path', weight: 0.2 },
          { name: 'label', weight: 0.1 },
        ],
        threshold: 0.4,
        ignoreLocation: true,
        includeScore: true,
      }),
    [targets]
  )

  const results = useMemo(() => {
    const q = input.trim()
    if (!q) {
      // Empty query → frecent targets (top 12)
      const scored = targets
        .map((t) => ({ t, s: blend(frecencyKey(t), 0) }))
        .filter((x) => x.s > 0)
        .sort((a, b) => b.s - a.s)
        .slice(0, 12)
        .map((x) => x.t)
      return scored
    }
    return fuse
      .search(q)
      .map(({ item, score = 1 }) => ({
        item,
        // Lower fuse score = better; invert into 0..1 relevance
        blended: blend(frecencyKey(item), 1 - score),
      }))
      .sort((a, b) => b.blended - a.blended)
      .slice(0, 24)
      .map((x) => x.item)
  }, [input, fuse, blend, targets])

  const viewMatches = useMemo(() => {
    const q = input.trim().toLowerCase()
    if (!q) return views.slice(0, 5)
    return views
      .filter(
        (v) =>
          v.name.toLowerCase().includes(q) ||
          v.path.toLowerCase().includes(q) ||
          (v.entity?.toLowerCase().includes(q) ?? false)
      )
      .slice(0, 8)
  }, [views, input])

  // Reset input when opened
  useEffect(() => {
    if (open) setInput('')
  }, [open])

  const iconFor = (t: CommandTarget) =>
    t.kind === 'entity' ? (
      <TableIcon className="h-4 w-4 text-muted-foreground" />
    ) : (
      <Database className="h-4 w-4 text-muted-foreground" />
    )

  const rawQuery = input.trim()
  const lookedLikeQuery =
    supportsSql &&
    currentEntity &&
    /^(select|where|from|\$|\{|[a-z_]+\s*[=<>!~])/i.test(rawQuery)

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <CommandInput
        placeholder={
          currentEntity
            ? `Search tables, views, recents — or type a filter for ${currentEntity}`
            : 'Search tables, collections, recents…'
        }
        value={input}
        onValueChange={setInput}
      />
      <CommandList>
        <CommandEmpty>No matches.</CommandEmpty>

        {lookedLikeQuery && onRunRawQuery && (
          <CommandGroup heading="Run on current entity">
            <CommandItem
              value={`run-raw-${rawQuery}`}
              onSelect={() => {
                onRunRawQuery(rawQuery)
                onOpenChange(false)
              }}
            >
              <Terminal className="h-4 w-4" />
              <span className="font-mono text-xs truncate max-w-[40ch]">
                {rawQuery}
              </span>
              <CommandShortcut>↵</CommandShortcut>
            </CommandItem>
          </CommandGroup>
        )}

        {viewMatches.length > 0 && (
          <>
            <CommandGroup heading="Saved views">
              {viewMatches.map((v) => (
                <CommandItem
                  key={v.id}
                  value={`view-${v.id}-${v.name}`}
                  onSelect={() => {
                    record(viewKey(v))
                    onOpenView(v)
                    onOpenChange(false)
                  }}
                >
                  <Bookmark
                    className={cn(
                      'h-4 w-4',
                      v.pinned ? 'text-primary' : 'text-muted-foreground'
                    )}
                  />
                  <span className="flex-1 truncate">{v.name}</span>
                  <span className="text-xs text-muted-foreground truncate max-w-[28ch]">
                    {v.path}
                    {v.entity && ` / ${v.entity}`}
                  </span>
                </CommandItem>
              ))}
            </CommandGroup>
            <CommandSeparator />
          </>
        )}

        {results.length > 0 && (
          <CommandGroup
            heading={input.trim() ? 'Matches' : 'Recent'}
          >
            {!input.trim() && results.length > 0 && (
              <div className="px-2 py-1 text-[10px] uppercase tracking-wider text-muted-foreground/70 flex items-center gap-1">
                <Clock className="h-3 w-3" />
                ranked by frequency + recency
              </div>
            )}
            {results.map((t) => {
              const freq = store[frecencyKey(t)]
              return (
                <CommandItem
                  key={t.id}
                  value={`target-${t.id}-${t.name}-${t.path}`}
                  onSelect={() => {
                    record(frecencyKey(t))
                    onJump(t)
                    onOpenChange(false)
                  }}
                >
                  {iconFor(t)}
                  <div className="flex-1 min-w-0">
                    <div className="truncate flex items-center gap-2">
                      <span className="font-medium">{t.name}</span>
                      {t.label && (
                        <Badge variant="outline" className="text-[10px] h-4 px-1">
                          {t.label}
                        </Badge>
                      )}
                    </div>
                    <div className="text-xs text-muted-foreground truncate">
                      {t.path}
                      {t.entity && ` / ${t.entity}`}
                    </div>
                  </div>
                  {freq && freq.count > 1 && (
                    <span
                      className="text-[10px] text-muted-foreground flex items-center gap-0.5"
                      title={`Used ${freq.count}×`}
                    >
                      <History className="h-3 w-3" />
                      {freq.count}
                    </span>
                  )}
                  <ExternalLink className="h-3 w-3 text-muted-foreground/60" />
                </CommandItem>
              )
            })}
          </CommandGroup>
        )}
      </CommandList>
      <div className="border-t px-3 py-2 flex items-center justify-between text-[10px] text-muted-foreground bg-muted/30">
        <span>
          <kbd className="px-1 py-0.5 rounded bg-background border font-mono">
            ↵
          </kbd>{' '}
          open ·{' '}
          <kbd className="px-1 py-0.5 rounded bg-background border font-mono">
            esc
          </kbd>{' '}
          close
        </span>
        <span>
          {targets.length} targets · {views.length} views
        </span>
      </div>
    </CommandDialog>
  )
}

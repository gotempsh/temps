// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Attribute facet sidebar (ADR-047 §5) — every attribute the line index has
 * seen in the current scope, expandable into its top values, each of which
 * becomes an `attr` predicate (`key=value`, `key!=value` or `key?`).
 *
 * Renders even when the ClickHouse line index is not configured: the
 * onboarding state names exactly what's missing and links to setup
 * (CLAUDE.md — unconfigured features must onboard, never disappear).
 */

import { useState } from 'react'
import { Link } from 'react-router'
import { ChevronDown, ChevronRight, MoreHorizontal, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import type { AnalyticsCapability } from '@/api/client/types.gen'
import {
  attrFacetValues,
  formatAttrExists,
  formatAttrPredicate,
  parseAttrPredicate,
  useGlobalLogAttrValues,
  useGlobalLogAttributeKeys,
  type GlobalLogFilters,
} from '@/hooks/useGlobalLogs'

/** Chips for the active `attr` predicates, shown in the query area. */
export function AttrPredicateChips({
  predicates,
  onRemove,
}: {
  predicates: string[]
  onRemove: (predicate: string) => void
}) {
  if (!predicates.length) return null
  return (
    <div className="flex flex-wrap items-center gap-1">
      {predicates.map((raw) => {
        const parsed = parseAttrPredicate(raw)
        const label = parsed
          ? parsed.op === '?'
            ? `${parsed.key} exists`
            : `${parsed.key}${parsed.op}${parsed.value}`
          : raw
        return (
          <div
            key={raw}
            className="flex max-w-full items-center rounded bg-secondary font-mono text-[11px]"
          >
            <span className="truncate py-1 pl-2">{label}</span>
            <Button
              variant="ghost"
              size="icon"
              className="size-6 shrink-0"
              aria-label={`Remove ${label}`}
              onClick={() => onRemove(raw)}
            >
              <X className="size-3" />
            </Button>
          </div>
        )
      })}
    </div>
  )
}

function AttrValueList({
  filters,
  attrKey,
  activePredicates,
  onAdd,
}: {
  filters: GlobalLogFilters
  attrKey: string
  activePredicates: string[]
  onAdd: (predicate: string) => void
}) {
  const query = useGlobalLogAttrValues(filters, attrKey, true)
  const values = attrFacetValues(query.data, attrKey)
  const total = Math.max(
    1,
    values.reduce((sum, item) => sum + item.count, 0)
  )
  if (query.isPending)
    return (
      <div className="space-y-1 py-1 pl-2">
        <Skeleton className="h-5 w-full" />
        <Skeleton className="h-5 w-full" />
      </div>
    )
  if (query.isError)
    return (
      <p className="py-1 pl-2 text-[11px] text-muted-foreground">
        Values could not be loaded.
      </p>
    )
  if (!values.length)
    return (
      <p className="py-1 pl-2 text-[11px] text-muted-foreground">
        No values in this window.
      </p>
    )
  return (
    <div className="pl-2">
      {values.map((item) => {
        const eq = formatAttrPredicate(attrKey, '=', item.value)
        const neq = formatAttrPredicate(attrKey, '!=', item.value)
        const active = activePredicates.includes(eq)
        return (
          <div
            key={item.value}
            className="group relative flex h-7 w-full items-center gap-1 overflow-hidden border-b"
          >
            <span
              aria-hidden="true"
              className="pointer-events-none absolute inset-y-1 left-0 bg-muted/70"
              style={{ width: `${(item.count / total) * 100}%` }}
            />
            <button
              type="button"
              disabled={active}
              className="relative flex h-full flex-1 items-center justify-between gap-2 px-2 text-left text-[11px] disabled:cursor-default disabled:opacity-70"
              onClick={() => onAdd(eq)}
              aria-label={`Filter ${attrKey} = ${item.value}`}
            >
              <span className="truncate">{item.value}</span>
              <span className="shrink-0 tabular-nums text-muted-foreground">
                {item.count}
              </span>
            </button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="relative size-6 shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100"
                  aria-label={`More filters for ${attrKey} = ${item.value}`}
                >
                  <MoreHorizontal className="size-3" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => onAdd(eq)}>
                  Filter to this value ({attrKey}={item.value})
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => onAdd(neq)}>
                  Exclude this value ({attrKey}!={item.value})
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )
      })}
    </div>
  )
}

export function LogAttributeSidebar({
  filters,
  capability,
  capabilityLoading,
  activePredicates,
  onAddPredicate,
}: {
  filters: GlobalLogFilters
  capability?: AnalyticsCapability
  capabilityLoading: boolean
  activePredicates: string[]
  onAddPredicate: (predicate: string) => void
}) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const configured = capability?.configured === true
  const keys = useGlobalLogAttributeKeys(filters, configured)

  return (
    <section aria-label="Attribute facets" className="border-t pt-3">
      <h3 className="mb-1 text-xs font-medium text-muted-foreground">
        Attributes
      </h3>
      {capabilityLoading ? (
        <Skeleton className="h-8 w-full" />
      ) : !configured ? (
        <div className="space-y-1.5 py-1">
          <p className="text-xs text-foreground">
            {capability?.reason ??
              'Attribute analytics are not configured on this instance.'}
          </p>
          <p className="text-[11px] text-muted-foreground">
            {capability?.example ??
              'Click any attribute your app logs to filter and chart by it.'}
          </p>
          <Link
            to={capability?.setup_path ?? '/settings/metrics-monitoring'}
            className="text-[11px] underline underline-offset-2 hover:text-foreground"
          >
            Configure in Settings
          </Link>
        </div>
      ) : keys.isPending ? (
        <div className="space-y-1 py-1">
          <Skeleton className="h-6 w-full" />
          <Skeleton className="h-6 w-full" />
          <Skeleton className="h-6 w-full" />
        </div>
      ) : keys.isError ? (
        <div className="space-y-1.5 py-1">
          <p className="text-xs text-muted-foreground">
            Attribute keys could not be loaded.
          </p>
          <Button
            variant="outline"
            size="sm"
            className="h-7 text-xs"
            onClick={() => void keys.refetch()}
          >
            Retry
          </Button>
        </div>
      ) : !keys.data?.keys.length ? (
        <p className="py-1 text-xs text-muted-foreground">
          No attributes seen in this window.
        </p>
      ) : (
        keys.data.keys.map((key) => {
          const isOpen = expanded.has(key.value)
          const existsPredicate = formatAttrExists(key.value)
          return (
            <div key={key.value}>
              <div className="flex h-7 w-full items-center">
                <button
                  type="button"
                  className="flex h-full flex-1 items-center gap-1 text-left text-[11px] hover:underline"
                  aria-expanded={isOpen}
                  onClick={() =>
                    setExpanded((previous) => {
                      const next = new Set(previous)
                      if (next.has(key.value)) next.delete(key.value)
                      else next.add(key.value)
                      return next
                    })
                  }
                >
                  {isOpen ? (
                    <ChevronDown className="size-3 shrink-0" />
                  ) : (
                    <ChevronRight className="size-3 shrink-0" />
                  )}
                  <span className="truncate font-mono">{key.value}</span>
                </button>
                <span className="shrink-0 pr-1 tabular-nums text-[11px] text-muted-foreground">
                  {key.count}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 shrink-0 px-1.5 text-[10px]"
                  disabled={activePredicates.includes(existsPredicate)}
                  onClick={() => onAddPredicate(existsPredicate)}
                >
                  exists
                </Button>
              </div>
              {isOpen && (
                <AttrValueList
                  filters={filters}
                  attrKey={key.value}
                  activePredicates={activePredicates}
                  onAdd={onAddPredicate}
                />
              )}
            </div>
          )
        })
      )}
    </section>
  )
}

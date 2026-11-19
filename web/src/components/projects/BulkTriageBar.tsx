// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Loader2, Merge, RotateCcw } from 'lucide-react'
import { IgnoreMenu } from '@/components/error-tracking/IgnoreMenu'
import { ResolveMenu } from '@/components/error-tracking/ResolveMenu'
import type { IgnoreConditions } from '@/components/error-tracking/ignore-presets'

export type BulkTriageAction = 'resolve' | 'unresolve' | 'ignore'

interface BulkTriageBarProps {
  /** Group ids on the page the user can see right now. */
  pageGroupIds: number[]
  selectedGroupIds: number[]
  onSelectionChange: (ids: number[]) => void
  /**
   * `ignore` carries when the groups come back; omitted, a plain ignore.
   * `inNextRelease` goes with `resolve` only.
   */
  onAction: (
    action: BulkTriageAction,
    ignore?: IgnoreConditions,
    inNextRelease?: boolean
  ) => void
  /** Merge every selected group into one; needs at least two selected. */
  onMerge: () => void
  isPending: boolean
}

/**
 * Triage the selected error groups in one request (ADR 074, D1). The bar
 * stays in place with the page's select-all control when nothing is selected,
 * so selecting many groups is one click away instead of a hidden gesture.
 */
export function BulkTriageBar({
  pageGroupIds,
  selectedGroupIds,
  onSelectionChange,
  onAction,
  onMerge,
  isPending,
}: BulkTriageBarProps) {
  const selectedOnPage = selectedGroupIds.filter((id) =>
    pageGroupIds.includes(id)
  )
  const allSelected =
    pageGroupIds.length > 0 && selectedOnPage.length === pageGroupIds.length
  const count = selectedGroupIds.length

  return (
    <div className="flex flex-col gap-2 rounded-md border border-border/60 bg-muted/30 px-3 py-2 sm:flex-row sm:items-center sm:justify-between">
      <label className="flex items-center gap-2 text-xs text-muted-foreground">
        <Checkbox
          checked={allSelected}
          onCheckedChange={(checked) =>
            onSelectionChange(checked === true ? pageGroupIds : [])
          }
          aria-label="Select every error group on this page"
        />
        {count > 0
          ? `${count.toLocaleString()} selected`
          : 'Select groups to resolve, ignore or reopen together'}
      </label>
      <div className="flex flex-wrap items-center gap-2">
        {isPending && (
          <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
        )}
        <ResolveMenu
          compactLabel
          disabled={count === 0 || isPending}
          onResolve={(inNextRelease) =>
            onAction('resolve', undefined, inNextRelease)
          }
        />
        <IgnoreMenu
          compactLabel
          disabled={count === 0 || isPending}
          onIgnore={(conditions) => onAction('ignore', conditions)}
        />
        <Button
          size="sm"
          variant="outline"
          disabled={count === 0 || isPending}
          onClick={() => onAction('unresolve')}
        >
          <RotateCcw className="h-3.5 w-3.5" />
          <span className="max-sm:sr-only">Reopen</span>
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={count < 2 || isPending}
          onClick={onMerge}
          title={
            count < 2
              ? 'Select two or more groups to merge them into one'
              : undefined
          }
        >
          <Merge className="h-3.5 w-3.5" />
          <span className="max-sm:sr-only">Merge</span>
        </Button>
        {count > 0 && (
          <Button
            size="sm"
            variant="ghost"
            disabled={isPending}
            onClick={() => onSelectionChange([])}
          >
            Clear
          </Button>
        )}
      </div>
    </div>
  )
}

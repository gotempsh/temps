// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId } from 'react'
import { AlertTriangle, Loader2, Tag, Zap } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import type { AttributeKeyState } from '@/lib/attribute-facets'
import { cn } from '@/lib/utils'

/** The key and value inputs. `facetKeys` feed a native suggestion list, so a
 *  facet is one click away but any key can still be typed. */
export function AttributeFilterControls({
  attrKey,
  attrValue,
  facetKeys,
  onKeyChange,
  onValueChange,
}: {
  attrKey: string
  attrValue: string
  facetKeys: readonly string[]
  onKeyChange: (key: string) => void
  onValueChange: (value: string) => void
}) {
  const listId = useId()
  return (
    <>
      <div className="relative w-full sm:w-52">
        <Tag
          aria-hidden="true"
          className="pointer-events-none absolute start-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground"
        />
        <Input
          className="h-9 ps-8 text-sm"
          value={attrKey}
          list={listId}
          onChange={(event) => onKeyChange(event.target.value)}
          aria-label="Attribute key"
          placeholder="Attribute, e.g. http.route"
          autoComplete="off"
          spellCheck={false}
        />
        <datalist id={listId}>
          {facetKeys.map((key) => (
            <option key={key} value={key} />
          ))}
        </datalist>
      </div>
      {attrKey.trim() && (
        <Input
          className="h-9 w-full text-sm sm:w-44"
          value={attrValue}
          onChange={(event) => onValueChange(event.target.value)}
          aria-label={`Value for ${attrKey.trim()}`}
          placeholder={`Value for ${attrKey.trim()}…`}
          autoComplete="off"
        />
      )}
    </>
  )
}

/**
 * Says what the typed key means for speed and completeness, and offers the
 * fix. Renders nothing when there is nothing useful to add.
 */
export function AttributeFilterNotice({
  state,
  attrKey,
  facetedOnly,
  hasFacets,
  creationBlocker,
  creating,
  onCreate,
  onRetry,
  className,
}: {
  state: AttributeKeyState
  attrKey: string
  facetedOnly: boolean
  hasFacets: boolean
  creationBlocker: string | null
  creating: boolean
  onCreate: () => void
  /** Reloads the facet list after it failed to load. */
  onRetry?: () => void
  className?: string
}) {
  const key = attrKey.trim()
  const body = noticeBody(state, key, facetedOnly, hasFacets)
  if (!body) return null
  const offerCreate = state.kind === 'unfaceted'
  return (
    <div
      role="status"
      className={cn(
        'flex flex-col gap-2 rounded-md border px-3 py-2 text-xs sm:flex-row sm:items-center',
        body.tone === 'warning' &&
          'border-amber-500/40 bg-amber-500/5 text-foreground',
        body.tone === 'error' &&
          'border-destructive/40 bg-destructive/5 text-foreground',
        body.tone === 'info' && 'bg-muted/40 text-muted-foreground',
        className
      )}
    >
      <body.Icon
        aria-hidden="true"
        className={cn(
          'size-3.5 shrink-0',
          body.tone === 'warning' && 'text-amber-700 dark:text-amber-600',
          body.tone === 'error' && 'text-destructive'
        )}
      />
      <p className="min-w-0 flex-1">{body.text}</p>
      {state.kind === 'unavailable' && onRetry && (
        <Button
          type="button"
          size="sm"
          variant="outline"
          className="h-7 text-xs"
          onClick={onRetry}
        >
          Retry
        </Button>
      )}
      {offerCreate && (
        <div className="flex flex-col gap-1 sm:items-end">
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="h-7 gap-1.5 text-xs"
            disabled={creating || creationBlocker !== null}
            onClick={onCreate}
          >
            {creating && <Loader2 className="size-3 animate-spin" />}
            Create facet
          </Button>
          {creationBlocker && (
            <span className="text-muted-foreground">{creationBlocker}</span>
          )}
        </div>
      )}
    </div>
  )
}

type NoticeBody = {
  tone: 'info' | 'warning' | 'error'
  Icon: typeof Zap
  text: string
}

function noticeBody(
  state: AttributeKeyState,
  key: string,
  facetedOnly: boolean,
  hasFacets: boolean
): NoticeBody | null {
  switch (state.kind) {
    case 'empty':
      // An unconfigured feature must say what it would do, not vanish.
      return hasFacets
        ? null
        : {
            tone: 'info',
            Icon: Zap,
            text: 'Filter spans by an attribute. Turn an attribute into a facet and filtering on it is instant, even over long time ranges.',
          }
    case 'loading':
      return null
    case 'unavailable':
      return {
        tone: 'error',
        Icon: AlertTriangle,
        text: facetedOnly
          ? `Couldn't load the facet list, so "${key}" cannot be checked and the filter is not applied. Retry to filter on it.`
          : `Couldn't load the facet list, so "${key}" cannot be checked. The filter is still applied, but it may read the attributes of every span in the time range.`,
      }
    case 'invalid':
      return { tone: 'error', Icon: AlertTriangle, text: state.reason }
    case 'ready':
      return {
        tone: 'info',
        Icon: Zap,
        text: `"${key}" is an indexed facet, so this filter is fast.`,
      }
    case 'indexing':
      return {
        tone: 'warning',
        Icon: Loader2,
        text: `"${key}" is still being indexed. Filtering is fast, but spans stored before the facet was created may be missing until indexing finishes.`,
      }
    case 'failed':
      return {
        tone: 'error',
        Icon: AlertTriangle,
        text: `Indexing "${key}" failed${state.facet.error_message ? `: ${state.facet.error_message}` : ''}. Results may be incomplete.`,
      }
    case 'removing':
      return {
        tone: 'info',
        Icon: Loader2,
        text: `"${key}" is being removed as a facet. The filter is not applied.`,
      }
    case 'unfaceted':
      return facetedOnly
        ? {
            tone: 'warning',
            Icon: AlertTriangle,
            text: `"${key}" is not a facet, so it cannot be filtered here: that would read the attributes of every span in the project. Create a facet to filter on it; existing spans are indexed in the background.`,
          }
        : {
            tone: 'warning',
            Icon: AlertTriangle,
            text: `"${key}" is not a facet, so this filter reads the attributes of every span in the time range and is slow on long ranges. Create a facet to make it fast; existing spans are indexed in the background.`,
          }
  }
}

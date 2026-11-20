// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { FlagResponse } from '@/api/client'
import { updateFlagMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useMutation } from '@tanstack/react-query'
import { Loader2, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { FlagValueField } from './FlagValueField'
import {
  MAX_VARIANTS,
  referencedVariants,
  validateVariantKey,
  type FlagRule,
  type FlagVariant,
} from './flag-rules'
import {
  asFlagValueType,
  flagErrorMessage,
  parseFlagValue,
  toEditableString,
} from './flag-value'
import { useDraftBaseline } from './draft-baseline'

interface DraftVariant {
  id: string
  key: string
  valueText: string
  description: string
}

let sequence = 0
function draftId(): string {
  sequence += 1
  return `variant-${sequence}`
}

/**
 * Named values a rule can serve or split traffic across, e.g. `control` and
 * `test`. Saved together with the flag definition's revision so a concurrent
 * edit is refused instead of overwritten. A variant still served by a rule in
 * any environment cannot be removed; the server says which rule uses it.
 */
/** A draft list's content, ignoring the client-only row ids. */
function draftsKey(drafts: DraftVariant[]): string {
  return JSON.stringify(
    drafts.map(({ key, valueText, description }) => [
      key,
      valueText,
      description,
    ])
  )
}

export function FlagVariantsEditor({
  projectId,
  flag,
  onChanged,
}: {
  projectId: number
  flag: FlagResponse
  onChanged: () => void
}) {
  const valueType = asFlagValueType(flag.value_type)
  const stored = (flag.variants ?? []) as FlagVariant[]
  const inUse = referencedVariants(
    (flag.environments ?? []).flatMap((env) => (env.rules ?? []) as FlagRule[])
  )

  const seed = (variants: FlagVariant[]): DraftVariant[] =>
    variants.map((variant) => ({
      id: draftId(),
      key: variant.key,
      valueText: toEditableString(variant.value, valueType),
      description: variant.description ?? '',
    }))
  const [drafts, setDrafts] = useState<DraftVariant[]>(() => seed(stored))
  const [error, setError] = useState<string | null>(null)
  // Stays mounted across refetches; this is the revision the drafts are
  // based on, so a save never carries a newer one than they were made from.
  const baseRevision = useDraftBaseline(
    flag.updated_at,
    stored,
    (variants) => draftsKey(drafts) !== draftsKey(seed(variants)),
    (variants) => setDrafts(seed(variants))
  )

  const save = useMutation({
    ...updateFlagMutation(),
    onSuccess: () => {
      toast.success('Variants saved')
      onChanged()
    },
    onError: (failure) => {
      const status = (failure as { status?: number } | null)?.status
      setError(
        status === 409
          ? 'The flag changed after you opened it. Close and reopen it, then apply your change again.'
          : flagErrorMessage(failure, 'Could not save the variants')
      )
    },
  })

  const set = (id: string, change: Partial<DraftVariant>) =>
    setDrafts((current) =>
      current.map((draft) =>
        draft.id === id ? { ...draft, ...change } : draft
      )
    )

  const submit = () => {
    const variants: FlagVariant[] = []
    const seen = new Set<string>()
    for (const [index, draft] of drafts.entries()) {
      const key = draft.key.trim()
      const keyError = validateVariantKey(key)
      if (keyError) {
        setError(`Variant ${index + 1}: ${keyError}`)
        return
      }
      if (seen.has(key)) {
        setError(`Variant "${key}" is declared twice.`)
        return
      }
      seen.add(key)
      const parsed = parseFlagValue(draft.valueText, valueType)
      if (!parsed.ok) {
        setError(`Variant "${key}": ${parsed.error}`)
        return
      }
      variants.push({
        key,
        value: parsed.value,
        ...(draft.description.trim()
          ? { description: draft.description.trim() }
          : {}),
      })
    }
    setError(null)
    save.mutate({
      path: { project_id: projectId, key: flag.key },
      body: {
        variants: variants as never,
        expected_updated_at: baseRevision,
      },
    })
  }

  return (
    <div className="space-y-3">
      <div className="space-y-1">
        <p className="text-sm font-medium">Variants</p>
        <p className="text-xs text-muted-foreground">
          Named values for experiments and multi-way rollouts. A rule can serve
          one, or split users across several.
        </p>
      </div>

      {drafts.length === 0 && (
        <p className="text-xs text-muted-foreground">
          No variants. For an A/B test add two, e.g. <code>control</code> and{' '}
          <code>test</code>.
        </p>
      )}

      {drafts.map((draft, index) => (
        <div key={draft.id} className="space-y-2 rounded-md border p-3">
          <div className="flex items-center gap-2">
            <Input
              aria-label={`Variant ${index + 1} key`}
              value={draft.key}
              onChange={(e) => set(draft.id, { key: e.target.value })}
              placeholder="control"
              className="font-mono"
              spellCheck={false}
            />
            <Button
              type="button"
              size="icon"
              variant="ghost"
              aria-label={`Remove variant ${draft.key || index + 1}`}
              title={
                inUse.has(draft.key)
                  ? 'Served by a rule; remove it from the rules first'
                  : undefined
              }
              disabled={inUse.has(draft.key)}
              onClick={() =>
                setDrafts((current) => current.filter((d) => d.id !== draft.id))
              }
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          </div>
          <FlagValueField
            id={`${draft.id}-value`}
            label="Value"
            type={valueType}
            value={draft.valueText}
            onChange={(next) => set(draft.id, { valueText: next })}
          />
          <Input
            aria-label={`Variant ${index + 1} description`}
            value={draft.description}
            onChange={(e) => set(draft.id, { description: e.target.value })}
            placeholder="Description (optional)"
          />
        </div>
      ))}

      {error && <p className="text-xs text-destructive">{error}</p>}

      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          size="sm"
          variant="ghost"
          disabled={drafts.length >= MAX_VARIANTS}
          onClick={() =>
            setDrafts((current) => [
              ...current,
              {
                id: draftId(),
                key:
                  current.length === 0
                    ? 'control'
                    : current.length === 1
                      ? 'test'
                      : '',
                valueText: toEditableString(flag.default_value, valueType),
                description: '',
              },
            ])
          }
        >
          <Plus className="mr-1.5 h-4 w-4" />
          Add variant
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={submit}
          disabled={save.isPending}
        >
          {save.isPending && (
            <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
          )}
          Save variants
        </Button>
      </div>
    </div>
  )
}

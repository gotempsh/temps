// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, FlagResponse } from '@/api/client'
import { setFlagRulesMutation } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useMutation } from '@tanstack/react-query'
import {
  ArrowDown,
  ArrowUp,
  Loader2,
  Plus,
  Trash2,
  TriangleAlert,
} from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { FlagValueField } from './FlagValueField'
import {
  IMPLICIT_ATTRIBUTES,
  MAX_CONDITIONS,
  MAX_RULES,
  OPERATORS,
  describeRule,
  emptyDraft,
  evenSplit,
  setSplitWeight,
  fromDraft,
  operandShape,
  toDraft,
  type DraftCondition,
  type DraftRule,
  type FlagRule,
  type FlagVariant,
} from './flag-rules'
import {
  asFlagValueType,
  environmentOverride,
  flagErrorMessage,
  toEditableString,
} from './flag-value'
import { useDraftBaseline } from './draft-baseline'

interface FlagRulesEditorProps {
  projectId: number
  flag: FlagResponse
  environment: EnvironmentResponse
  onChanged: () => void
}

/**
 * Ordered targeting rules for one flag in one environment.
 *
 * Rules are edited as a draft and saved together: the server replaces the
 * whole list, and refuses the save with 409 when someone else changed the
 * environment since this sheet loaded it. Keyed by its parent on the server
 * state, so a successful save (or a reload after a conflict) re-seeds the
 * draft instead of syncing it in an effect.
 */
/** A draft list's content, ignoring the client-only ids. */
function rulesKey(drafts: DraftRule[]): string {
  return JSON.stringify(drafts, (name, value: unknown) =>
    name === 'id' ? undefined : value
  )
}

export function FlagRulesEditor({
  projectId,
  flag,
  environment,
  onChanged,
}: FlagRulesEditorProps) {
  const valueType = asFlagValueType(flag.value_type)
  const override = environmentOverride(flag, environment.id)
  const stored = (override?.rules ?? []) as FlagRule[]
  const variants = (flag.variants ?? []) as FlagVariant[]

  const [drafts, setDrafts] = useState<DraftRule[]>(() =>
    stored.map((rule) => toDraft(rule, valueType))
  )
  const [editing, setEditing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Stays mounted across refetches (saving variants or an environment value
  // refetches the flag), so an open edit survives; this is the row revision
  // the drafts are based on. A closed editor takes every fetched revision:
  // after "Discard and reload" or a save its drafts hold rules from before
  // the refetch, and must not be carried forward under the new revision.
  const [baseRevision, rulesSaved] = useDraftBaseline(
    override?.updated_at ?? null,
    stored,
    (rules) =>
      rulesKey(drafts) !==
      rulesKey(rules.map((rule) => toDraft(rule, valueType))),
    (rules) => setDrafts(rules.map((rule) => toDraft(rule, valueType))),
    editing
  )

  const save = useMutation({
    ...setFlagRulesMutation(),
    onSuccess: () => {
      toast.success(`Rules saved for ${environment.name}`)
      rulesSaved()
      setEditing(false)
      onChanged()
    },
    onError: (failure) => {
      const status = (failure as { status?: number } | null)?.status
      if (status === 409) {
        setError(
          'Someone else changed these rules after you opened them. Discard your edits to see their version, then apply your change again.'
        )
        return
      }
      setError(flagErrorMessage(failure, 'Could not save the rules'))
    },
  })

  const update = (id: string, change: Partial<DraftRule>) =>
    setDrafts((current) =>
      current.map((draft) =>
        draft.id === id ? { ...draft, ...change } : draft
      )
    )

  const move = (index: number, by: -1 | 1) =>
    setDrafts((current) => {
      const next = [...current]
      const target = index + by
      if (target < 0 || target >= next.length) return current
      ;[next[index], next[target]] = [next[target], next[index]]
      return next
    })

  const submit = () => {
    const rules: FlagRule[] = []
    for (const [index, draft] of drafts.entries()) {
      const parsed = fromDraft(draft, valueType, variants, index + 1)
      if (!parsed.ok) {
        setError(parsed.error)
        return
      }
      rules.push(parsed.rule)
    }
    setError(null)
    save.mutate({
      path: {
        project_id: projectId,
        key: flag.key,
        environment_id: environment.id,
      },
      body: {
        rules: rules as never,
        // Absent when the environment had no row yet: the server then
        // refuses the save if one appeared meanwhile.
        expected_updated_at: baseRevision,
      },
    })
  }

  const cancel = () => {
    setDrafts(stored.map((rule) => toDraft(rule, valueType)))
    setError(null)
    setEditing(false)
  }

  if (!editing) {
    return (
      <div className="space-y-2">
        <div className="flex items-center justify-between gap-2">
          <p className="text-xs font-medium text-muted-foreground">
            Targeting rules
          </p>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => {
              // Always start from what is stored now.
              setDrafts(stored.map((rule) => toDraft(rule, valueType)))
              setEditing(true)
            }}
          >
            {stored.length === 0 ? 'Add rules' : 'Edit rules'}
          </Button>
        </div>
        {override && !override.rules_valid && (
          <Alert variant="destructive">
            <TriangleAlert className="h-4 w-4" />
            <AlertDescription>
              The stored rules could not be read, so this environment serves the
              default with reason ERROR. Edit and save them to fix it.
            </AlertDescription>
          </Alert>
        )}
        {stored.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            None. Everyone gets the value above. Rules let you serve a different
            value to some users, or to a percentage of them.
          </p>
        ) : (
          <ol className="space-y-1">
            {stored.map((rule, index) => (
              <li
                // Rules have no identity of their own; their position is it.
                key={index}
                className="flex gap-2 text-xs"
              >
                <Badge variant="outline" className="shrink-0 tabular-nums">
                  {index + 1}
                </Badge>
                <span className="min-w-0 break-words">
                  {rule.description ? `${rule.description}: ` : ''}
                  {describeRule(rule, valueType)}
                </span>
              </li>
            ))}
          </ol>
        )}
      </div>
    )
  }

  return (
    <div className="space-y-3 rounded-md border border-dashed p-3">
      <p className="text-xs text-muted-foreground">
        Checked top to bottom; the first rule that matches decides the value. A
        user who matches no rule gets the environment value.
      </p>

      {drafts.map((draft, index) => (
        <RuleCard
          key={draft.id}
          draft={draft}
          position={index + 1}
          count={drafts.length}
          flag={flag}
          variants={variants}
          onChange={(change) => update(draft.id, change)}
          onMove={(by) => move(index, by)}
          onRemove={() =>
            setDrafts((current) => current.filter((d) => d.id !== draft.id))
          }
        />
      ))}

      <Button
        type="button"
        size="sm"
        variant="outline"
        disabled={drafts.length >= MAX_RULES}
        onClick={() =>
          setDrafts((current) => [
            ...current,
            emptyDraft(valueType, flag.default_value),
          ])
        }
      >
        <Plus className="mr-1.5 h-4 w-4" />
        Add rule
      </Button>

      {error && <p className="text-xs text-destructive">{error}</p>}

      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          size="sm"
          onClick={submit}
          disabled={save.isPending}
        >
          {save.isPending && (
            <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
          )}
          Save rules
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          onClick={cancel}
          disabled={save.isPending}
        >
          Cancel
        </Button>
        {error?.startsWith('Someone else') && (
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => {
              // Dropping the drafts is what lets the editor adopt the server
              // revision: a dirty draft keeps its old one on purpose.
              cancel()
              onChanged()
            }}
          >
            Discard and reload
          </Button>
        )}
      </div>
    </div>
  )
}

interface RuleCardProps {
  draft: DraftRule
  position: number
  count: number
  flag: FlagResponse
  variants: FlagVariant[]
  onChange: (change: Partial<DraftRule>) => void
  onMove: (by: -1 | 1) => void
  onRemove: () => void
}

function RuleCard({
  draft,
  position,
  count,
  flag,
  variants,
  onChange,
  onMove,
  onRemove,
}: RuleCardProps) {
  const valueType = asFlagValueType(flag.value_type)
  const idPrefix = `rule-${draft.id}`

  const setCondition = (index: number, change: Partial<DraftCondition>) =>
    onChange({
      conditions: draft.conditions.map((condition, i) =>
        i === index ? { ...condition, ...change } : condition
      ),
    })

  return (
    <div className="space-y-3 rounded-md border p-3">
      <div className="flex items-center justify-between gap-2">
        <p className="text-sm font-medium">Rule {position}</p>
        <div className="flex items-center gap-1">
          <Button
            type="button"
            size="icon"
            variant="ghost"
            aria-label={`Move rule ${position} up`}
            disabled={position === 1}
            onClick={() => onMove(-1)}
          >
            <ArrowUp className="h-4 w-4" />
          </Button>
          <Button
            type="button"
            size="icon"
            variant="ghost"
            aria-label={`Move rule ${position} down`}
            disabled={position === count}
            onClick={() => onMove(1)}
          >
            <ArrowDown className="h-4 w-4" />
          </Button>
          <Button
            type="button"
            size="icon"
            variant="ghost"
            aria-label={`Remove rule ${position}`}
            onClick={onRemove}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>

      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-description`}>Description</Label>
        <Input
          id={`${idPrefix}-description`}
          value={draft.description}
          onChange={(e) => onChange({ description: e.target.value })}
          placeholder="Optional, e.g. Beta testers"
        />
      </div>

      <div className="space-y-2">
        <p className="text-xs font-medium">
          {draft.conditions.length === 0
            ? 'Applies to everyone'
            : 'Applies when all of these hold'}
        </p>
        <datalist id={`${idPrefix}-attributes`}>
          {IMPLICIT_ATTRIBUTES.map((name) => (
            <option key={name} value={name} />
          ))}
        </datalist>
        {draft.conditions.map((condition, index) => (
          <div
            // Conditions are positional within their rule.
            key={index}
            className="flex flex-col gap-2 sm:flex-row sm:items-center"
          >
            <Input
              aria-label={`Rule ${position} condition ${index + 1} attribute`}
              list={`${idPrefix}-attributes`}
              value={condition.attribute}
              onChange={(e) =>
                setCondition(index, { attribute: e.target.value })
              }
              placeholder="attribute, e.g. plan"
              className="w-full sm:w-[140px]"
            />
            <Select
              value={condition.operator}
              onValueChange={(next) =>
                setCondition(index, {
                  operator: next as DraftCondition['operator'],
                })
              }
            >
              <SelectTrigger
                aria-label={`Rule ${position} condition ${index + 1} operator`}
                className="w-full sm:w-[150px]"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {OPERATORS.map((operator) => (
                  <SelectItem key={operator.value} value={operator.value}>
                    {operator.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {operandShape(condition.operator) !== 'none' && (
              <Input
                aria-label={`Rule ${position} condition ${index + 1} value`}
                value={condition.text}
                onChange={(e) => setCondition(index, { text: e.target.value })}
                placeholder={
                  operandShape(condition.operator) === 'list'
                    ? 'pro, team'
                    : 'value'
                }
                className="w-full min-w-0 flex-1"
              />
            )}
            <Button
              type="button"
              size="icon"
              variant="ghost"
              aria-label={`Remove condition ${index + 1} of rule ${position}`}
              onClick={() =>
                onChange({
                  conditions: draft.conditions.filter((_, i) => i !== index),
                })
              }
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          </div>
        ))}
        <Button
          type="button"
          size="sm"
          variant="ghost"
          disabled={draft.conditions.length >= MAX_CONDITIONS}
          onClick={() =>
            onChange({
              conditions: [
                ...draft.conditions,
                { attribute: '', operator: 'eq', text: '' },
              ],
            })
          }
        >
          <Plus className="mr-1.5 h-4 w-4" />
          Add condition
        </Button>
      </div>

      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-rollout`}>Rollout (%)</Label>
        <Input
          id={`${idPrefix}-rollout`}
          type="number"
          min={0}
          max={100}
          step="any"
          value={draft.rollout}
          onChange={(e) => onChange({ rollout: e.target.value })}
          placeholder="100"
          className="w-full tabular-nums sm:w-[120px]"
        />
        <p className="text-xs text-muted-foreground">
          Leave empty for everyone who matches. A partial rollout needs a
          subject (a user key or the visitor cookie); the same subject always
          lands on the same side.
        </p>
      </div>

      <div className="space-y-2">
        <Label htmlFor={`${idPrefix}-serve`}>Serve</Label>
        <Select
          value={draft.serveKind}
          onValueChange={(next) => {
            const kind = next as DraftRule['serveKind']
            onChange({
              serveKind: kind,
              // A boolean field shows "Off" for an empty draft, which would
              // then fail to save; start from the flag's default instead.
              ...(kind === 'value' &&
              valueType === 'bool' &&
              draft.valueText === ''
                ? {
                    valueText: toEditableString(flag.default_value, valueType),
                  }
                : {}),
              ...(kind === 'variant' && !draft.variant && variants[0]
                ? { variant: variants[0].key }
                : {}),
              ...(kind === 'split' && draft.split.length === 0
                ? { split: evenSplit(variants.map((v) => v.key)) }
                : {}),
            })
          }}
        >
          <SelectTrigger
            id={`${idPrefix}-serve`}
            className="w-full sm:w-[200px]"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="value">A value</SelectItem>
            <SelectItem value="variant" disabled={variants.length === 0}>
              A variant
            </SelectItem>
            <SelectItem value="split" disabled={variants.length < 2}>
              A split across variants
            </SelectItem>
          </SelectContent>
        </Select>
        {variants.length < 2 && (
          <p className="text-xs text-muted-foreground">
            Declare variants on the flag to serve them or split traffic between
            them.
          </p>
        )}

        {draft.serveKind === 'value' && (
          <FlagValueField
            id={`${idPrefix}-value`}
            label="Value"
            type={valueType}
            value={draft.valueText}
            onChange={(next) => onChange({ valueText: next })}
          />
        )}

        {draft.serveKind === 'variant' && (
          <Select
            value={draft.variant}
            onValueChange={(next) => onChange({ variant: next })}
          >
            <SelectTrigger
              aria-label={`Rule ${position} variant`}
              className="w-full sm:w-[200px]"
            >
              <SelectValue placeholder="Choose a variant" />
            </SelectTrigger>
            <SelectContent>
              {variants.map((variant) => (
                <SelectItem key={variant.key} value={variant.key}>
                  {variant.key}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}

        {draft.serveKind === 'split' && (
          <SplitEditor
            position={position}
            split={draft.split}
            variants={variants}
            onChange={(split) => onChange({ split })}
          />
        )}
      </div>
    </div>
  )
}

function SplitEditor({
  position,
  split,
  variants,
  onChange,
}: {
  position: number
  split: DraftRule['split']
  variants: FlagVariant[]
  onChange: (split: DraftRule['split']) => void
}) {
  const weights = new Map(split.map((arm) => [arm.variant, arm.weight]))
  const total = split.reduce((sum, arm) => sum + arm.weight, 0)

  const setWeight = (variant: string, raw: string) => {
    const weight = raw.trim() === '' ? 0 : Math.max(0, Math.round(Number(raw)))
    onChange(setSplitWeight(split, variant, weight))
  }

  return (
    <div className="space-y-2">
      {variants.map((variant) => (
        <div key={variant.key} className="flex items-center gap-2">
          <span className="w-32 truncate font-mono text-xs">{variant.key}</span>
          <Input
            aria-label={`Rule ${position} weight for ${variant.key}`}
            type="number"
            min={0}
            max={100}
            step={1}
            value={weights.get(variant.key) ?? 0}
            onChange={(e) => setWeight(variant.key, e.target.value)}
            className="w-24 tabular-nums"
          />
          <span className="text-xs text-muted-foreground">%</span>
        </div>
      ))}
      <p
        className={
          total === 100
            ? 'text-xs text-muted-foreground'
            : 'text-xs text-destructive'
        }
      >
        Total {total}%{total === 100 ? '' : ' (must be 100)'}
      </p>
      <Button
        type="button"
        size="sm"
        variant="ghost"
        onClick={() => onChange(evenSplit(variants.map((v) => v.key)))}
      >
        Split evenly
      </Button>
    </div>
  )
}

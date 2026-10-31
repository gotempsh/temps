// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import { Plus, Trash2 } from 'lucide-react'
import {
  changeField,
  newCondition,
  newGroup,
  removeNode,
  splitList,
  updateNode,
} from './expression'
import {
  fieldMeta,
  LIMITS,
  OPERATOR_LABELS,
  PRESENCE_OPERATORS,
  type FieldMeta,
} from './fields'
import type {
  Combinator,
  Condition,
  ConditionGroup,
  FieldId,
  Operator,
} from './types'

const FIELD_SECTIONS: Array<{ label: string; fields: FieldId[] }> = [
  { label: 'Request', fields: ['path', 'raw_path', 'host', 'method'] },
  {
    label: 'Headers and parameters',
    fields: ['header', 'query', 'cookie', 'user_agent'],
  },
  {
    label: 'Client',
    fields: [
      'ip',
      'country',
      'continent',
      'asn',
      'is_hosting_provider',
      'ja4',
      'bot_class',
    ],
  },
]

const COMBINATOR_LABELS: Record<Combinator, string> = {
  all: 'all',
  any: 'any',
  none: 'none',
}

interface ConditionBuilderProps {
  value: ConditionGroup
  onChange: (next: ConditionGroup) => void
  /** Validation messages keyed by node id. */
  issues: Map<string, string[]>
}

export function ConditionBuilder({
  value,
  onChange,
  issues,
}: ConditionBuilderProps) {
  return (
    <GroupEditor
      group={value}
      root={value}
      depth={1}
      onRootChange={onChange}
      issues={issues}
    />
  )
}

interface NodeProps {
  root: ConditionGroup
  depth: number
  onRootChange: (next: ConditionGroup) => void
  issues: Map<string, string[]>
}

function GroupEditor({
  group,
  root,
  depth,
  onRootChange,
  issues,
}: NodeProps & { group: ConditionGroup }) {
  const isRoot = group.id === root.id
  const groupIssues = issues.get(group.id) ?? []
  const combinatorId = `combinator-${group.id}`
  const append = (child: ConditionGroup['children'][number]) =>
    onRootChange(
      updateNode(root, group.id, (node) =>
        node.kind === 'group'
          ? { ...node, children: [...node.children, child] }
          : node
      )
    )

  return (
    <div
      role="group"
      aria-labelledby={`${combinatorId}-label`}
      className={cn('space-y-3', !isRoot && 'border-l-2 pl-4')}
    >
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span id={`${combinatorId}-label`} className="sr-only">
          {isRoot ? 'Rule conditions' : 'Condition group'}
        </span>
        <Label htmlFor={combinatorId} className="font-normal">
          Match
        </Label>
        <Select
          value={group.combinator}
          onValueChange={(combinator) =>
            onRootChange(
              updateNode(root, group.id, (node) =>
                node.kind === 'group'
                  ? { ...node, combinator: combinator as Combinator }
                  : node
              )
            )
          }
        >
          <SelectTrigger id={combinatorId} className="h-9 w-[96px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {(Object.keys(COMBINATOR_LABELS) as Combinator[]).map((c) => (
              <SelectItem key={c} value={c}>
                {COMBINATOR_LABELS[c]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <span>of the following</span>
        {!isRoot && (
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="ml-auto size-9"
            aria-label="Remove condition group"
            onClick={() => onRootChange(removeNode(root, group.id))}
          >
            <Trash2 />
          </Button>
        )}
      </div>

      {group.children.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {isRoot
            ? 'No conditions: this rule matches every request to the project.'
            : 'This group is empty.'}
        </p>
      ) : (
        <ul className="space-y-3">
          {group.children.map((child) => (
            <li key={child.id}>
              {child.kind === 'group' ? (
                <GroupEditor
                  group={child}
                  root={root}
                  depth={depth + 1}
                  onRootChange={onRootChange}
                  issues={issues}
                />
              ) : (
                <ConditionRow
                  condition={child}
                  root={root}
                  depth={depth}
                  onRootChange={onRootChange}
                  issues={issues}
                />
              )}
            </li>
          ))}
        </ul>
      )}

      {groupIssues.map((message) => (
        <p key={message} className="text-xs text-destructive">
          {message}
        </p>
      ))}

      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => append(newCondition())}
        >
          <Plus />
          Condition
        </Button>
        {depth < LIMITS.depth && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => append(newGroup('any'))}
          >
            <Plus />
            Group
          </Button>
        )}
      </div>
    </div>
  )
}

function ConditionRow({
  condition,
  root,
  onRootChange,
  issues,
}: NodeProps & { condition: Condition }) {
  const meta = fieldMeta(condition.field)
  const rowIssues = issues.get(condition.id) ?? []
  const invalid = rowIssues.length > 0
  const id = condition.id
  const set = (update: (c: Condition) => Condition) =>
    onRootChange(
      updateNode(root, id, (node) =>
        node.kind === 'condition' ? update(node) : node
      )
    )
  const presence = PRESENCE_OPERATORS.has(condition.op)
  const showCase = meta.caseFlag && !presence && meta.valueKind === 'text'

  return (
    <div className="space-y-1.5">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-start">
        <Label htmlFor={`field-${id}`} className="sr-only">
          Field
        </Label>
        <Select
          value={condition.field}
          onValueChange={(field) =>
            set((c) => changeField(c, field as FieldId))
          }
        >
          <SelectTrigger id={`field-${id}`} className="w-full sm:w-[200px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {FIELD_SECTIONS.map((section) => (
              <SelectGroup key={section.label}>
                <SelectLabel>{section.label}</SelectLabel>
                {section.fields.map((f) => (
                  <SelectItem key={f} value={f}>
                    {fieldMeta(f).label}
                  </SelectItem>
                ))}
              </SelectGroup>
            ))}
          </SelectContent>
        </Select>

        {meta.keyed && (
          <>
            <Label htmlFor={`key-${id}`} className="sr-only">
              {`${meta.keyed} name`}
            </Label>
            <Input
              id={`key-${id}`}
              value={condition.key ?? ''}
              placeholder={
                meta.keyed === 'header'
                  ? 'Header name'
                  : `${capitalize(meta.keyed)} name`
              }
              className="w-full font-mono sm:w-[180px]"
              aria-invalid={invalid && !(condition.key ?? '').trim()}
              onChange={(e) => set((c) => ({ ...c, key: e.target.value }))}
            />
          </>
        )}

        <Label htmlFor={`op-${id}`} className="sr-only">
          Operator
        </Label>
        <Select
          value={condition.op}
          onValueChange={(op) => set((c) => ({ ...c, op: op as Operator }))}
        >
          <SelectTrigger id={`op-${id}`} className="w-full sm:w-[170px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {meta.operators.map((op) => (
              <SelectItem key={op} value={op}>
                {OPERATOR_LABELS[op]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        {!presence && (
          <div className="min-w-0 flex-1 sm:min-w-[220px]">
            <ValueEditor
              condition={condition}
              meta={meta}
              invalid={invalid}
              onChange={(value) => set((c) => ({ ...c, value }))}
            />
          </div>
        )}

        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-10 self-end sm:self-start"
          aria-label={`Remove condition on ${meta.label.toLowerCase()}`}
          onClick={() => onRootChange(removeNode(root, id))}
        >
          <Trash2 />
        </Button>
      </div>

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
        {showCase && (
          <div className="flex items-center gap-2">
            <Checkbox
              id={`ci-${id}`}
              checked={!!condition.caseInsensitive}
              onCheckedChange={(checked) =>
                set((c) => ({ ...c, caseInsensitive: checked === true }))
              }
            />
            <Label htmlFor={`ci-${id}`} className="text-xs font-normal">
              Ignore case
            </Label>
          </div>
        )}
        <p className="text-xs text-muted-foreground">{meta.hint}</p>
      </div>
      {rowIssues.map((message) => (
        <p key={message} className="text-xs text-destructive">
          {message}
        </p>
      ))}
    </div>
  )
}

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1)

function ValueEditor({
  condition,
  meta,
  invalid,
  onChange,
}: {
  condition: Condition
  meta: FieldMeta
  invalid: boolean
  onChange: (value: string) => void
}) {
  const id = `value-${condition.id}`
  const label = (
    <Label htmlFor={id} className="sr-only">
      Value
    </Label>
  )

  if (meta.valueKind === 'boolean') {
    return (
      <>
        {label}
        <Select value={condition.value} onValueChange={onChange}>
          <SelectTrigger id={id}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="true">Yes</SelectItem>
            <SelectItem value="false">No</SelectItem>
          </SelectContent>
        </Select>
      </>
    )
  }

  if (meta.valueKind === 'options' && meta.options) {
    if (condition.op === 'eq') {
      return (
        <>
          {label}
          <Select value={condition.value || undefined} onValueChange={onChange}>
            <SelectTrigger id={id} aria-invalid={invalid}>
              <SelectValue
                placeholder={`Choose a ${meta.label.toLowerCase()}`}
              />
            </SelectTrigger>
            <SelectContent>
              {meta.options.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </>
      )
    }
    const selected = new Set(splitList(condition.value))
    const toggle = (value: string) => {
      const next = new Set(selected)
      if (next.has(value)) next.delete(value)
      else next.add(value)
      onChange(
        meta
          .options!.filter((o) => next.has(o.value))
          .map((o) => o.value)
          .join(', ')
      )
    }
    return (
      <div
        role="group"
        aria-label={`${meta.label} values`}
        className="flex flex-wrap gap-1.5"
      >
        {meta.options.map((o) => (
          <Button
            key={o.value}
            type="button"
            size="sm"
            variant={selected.has(o.value) ? 'default' : 'outline'}
            aria-pressed={selected.has(o.value)}
            onClick={() => toggle(o.value)}
          >
            {o.label}
          </Button>
        ))}
      </div>
    )
  }

  if (meta.valueKind === 'cidr') {
    return (
      <>
        {label}
        <Textarea
          id={id}
          rows={2}
          value={condition.value}
          placeholder={meta.placeholder}
          className="font-mono"
          aria-invalid={invalid}
          onChange={(e) => onChange(e.target.value)}
        />
      </>
    )
  }

  const list = condition.op === 'in'
  // `*` only means something to the pattern operator.
  const placeholder =
    condition.op === 'glob'
      ? meta.placeholder
      : (meta.placeholder ?? 'value').replace(/\*/g, '')
  const code = [
    'path',
    'raw_path',
    'header',
    'query',
    'cookie',
    'ja4',
    'host',
  ].includes(condition.field)
  return (
    <>
      {label}
      <Input
        id={id}
        value={condition.value}
        placeholder={list ? `${placeholder}, …` : placeholder}
        className={cn(code && 'font-mono')}
        aria-invalid={invalid}
        aria-describedby={list ? `${id}-list-hint` : undefined}
        onChange={(e) => onChange(e.target.value)}
      />
      {list && (
        <span id={`${id}-list-hint`} className="sr-only">
          Separate values with commas
        </span>
      )}
    </>
  )
}

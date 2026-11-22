// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
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
import { createContext, useContext, useState } from 'react'
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

const GROUP_LABELS: Record<Combinator, string> = {
  all: 'All of',
  any: 'Any of',
  none: 'None of',
}

interface ConditionBuilderProps {
  value: ConditionGroup
  onChange: (next: ConditionGroup) => void
  /** Validation messages keyed by node id. */
  issues: Map<string, string[]>
  /** Shows nested groups and "none of" matching. */
  advanced: boolean
}

/**
 * Conditions read as a sentence: rows joined by "and" or "or". Nested groups
 * and "none of" only appear in advanced mode, or when the rule already uses
 * them.
 */
/** Reports which field the user is working on, for the shared hint line. */
const FocusFieldContext = createContext<(field: FieldId) => void>(() => {})

export function ConditionBuilder({
  value,
  onChange,
  issues,
  advanced,
}: ConditionBuilderProps) {
  // One hint line under the builder, for the field last worked on. It sits
  // below every control and is never hidden again, so it cannot shift a
  // button out from under a click.
  const [hintField, setHintField] = useState<FieldId | null>(null)
  const hint = hintField ? fieldMeta(hintField) : null
  return (
    <FocusFieldContext.Provider value={setHintField}>
      <div className="space-y-1">
        <GroupEditor
          group={value}
          root={value}
          depth={1}
          onRootChange={onChange}
          issues={issues}
          advanced={advanced}
        />
        {hint && (
          <p className="px-1 text-xs text-muted-foreground" aria-live="polite">
            <span className="font-medium text-foreground">{hint.label}:</span>{' '}
            {hint.hint}
          </p>
        )}
      </div>
    </FocusFieldContext.Provider>
  )
}

interface NodeProps {
  root: ConditionGroup
  depth: number
  onRootChange: (next: ConditionGroup) => void
  issues: Map<string, string[]>
  advanced: boolean
}

function GroupEditor({
  group,
  root,
  depth,
  onRootChange,
  issues,
  advanced,
}: NodeProps & { group: ConditionGroup }) {
  const isRoot = group.id === root.id
  const groupIssues = issues.get(group.id) ?? []
  const combinatorId = `combinator-${group.id}`
  // A header picks all/any/none for nested groups and for an inverted root.
  // Otherwise the connectors between rows pick "and" or "or".
  const header = !isRoot || group.combinator === 'none'
  const setCombinator = (combinator: Combinator) =>
    onRootChange(
      updateNode(root, group.id, (node) =>
        node.kind === 'group' ? { ...node, combinator } : node
      )
    )
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
      aria-label={isRoot ? 'Rule conditions' : 'Condition group'}
      className={cn(
        'space-y-2',
        !isRoot && 'rounded-md border border-dashed bg-muted/30 p-3'
      )}
    >
      {header && (
        <div className="flex items-center gap-2 text-sm">
          <Label htmlFor={combinatorId} className="sr-only">
            Group matches
          </Label>
          <Select
            value={group.combinator}
            onValueChange={(c) => setCombinator(c as Combinator)}
          >
            <SelectTrigger id={combinatorId} className="h-8 w-[110px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {(Object.keys(GROUP_LABELS) as Combinator[]).map((c) => (
                <SelectItem key={c} value={c}>
                  {GROUP_LABELS[c]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <span className="text-muted-foreground">these</span>
          {!isRoot && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="ml-auto size-8"
              aria-label="Remove condition group"
              onClick={() => onRootChange(removeNode(root, group.id))}
            >
              <Trash2 />
            </Button>
          )}
        </div>
      )}

      {group.children.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {isRoot
            ? 'No conditions: the rule applies to every request.'
            : 'This group is empty.'}
        </p>
      ) : (
        <ul className="space-y-2">
          {group.children.map((child, index) => (
            <li key={child.id} className="space-y-2">
              {index > 0 && (
                <Connector
                  id={`${combinatorId}-${index}`}
                  combinator={group.combinator}
                  editable={!header}
                  onChange={setCombinator}
                />
              )}
              {child.kind === 'group' ? (
                <GroupEditor
                  group={child}
                  root={root}
                  depth={depth + 1}
                  onRootChange={onRootChange}
                  issues={issues}
                  advanced={advanced}
                />
              ) : (
                <ConditionRow
                  condition={child}
                  root={root}
                  depth={depth}
                  onRootChange={onRootChange}
                  issues={issues}
                  advanced={advanced}
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

      <div className="flex flex-wrap gap-1">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="text-muted-foreground"
          onClick={() => append(newCondition())}
        >
          <Plus />
          Add condition
        </Button>
        {advanced && depth < LIMITS.depth && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="text-muted-foreground"
            onClick={() => append(newGroup('any'))}
          >
            <Plus />
            Add group
          </Button>
        )}
      </div>
    </div>
  )
}

/** The word between two rows. In a plain list it switches and/or. */
function Connector({
  id,
  combinator,
  editable,
  onChange,
}: {
  id: string
  combinator: Combinator
  editable: boolean
  onChange: (next: Combinator) => void
}) {
  const word = combinator === 'all' ? 'and' : 'or'
  if (!editable) {
    return (
      <p className="pl-1 text-xs font-medium uppercase text-muted-foreground">
        {word}
      </p>
    )
  }
  return (
    <div>
      <Label htmlFor={id} className="sr-only">
        Join conditions with
      </Label>
      <Select
        value={combinator}
        onValueChange={(c) => onChange(c as Combinator)}
      >
        <SelectTrigger
          id={id}
          className="h-7 w-[72px] border-dashed text-xs font-medium uppercase"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="all">and</SelectItem>
          <SelectItem value="any">or</SelectItem>
        </SelectContent>
      </Select>
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
  const focusField = useContext(FocusFieldContext)

  return (
    <div className="space-y-1" onFocus={() => focusField(condition.field)}>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start">
        <Label htmlFor={`field-${id}`} className="sr-only">
          Field
        </Label>
        <Select
          value={condition.field}
          onValueChange={(field) => {
            set((c) => changeField(c, field as FieldId))
            focusField(field as FieldId)
          }}
        >
          <SelectTrigger id={`field-${id}`} className="w-full sm:w-[180px]">
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
              className="w-full font-mono sm:w-[160px]"
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
          <SelectTrigger id={`op-${id}`} className="w-full sm:w-[160px]">
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

        {presence ? (
          <div className="hidden flex-1 sm:block" />
        ) : (
          <div className="min-w-0 flex-1">
            <ValueEditor
              condition={condition}
              meta={meta}
              invalid={invalid}
              onChange={(value) => set((c) => ({ ...c, value }))}
            />
          </div>
        )}

        <div className="flex shrink-0 items-center gap-1 self-end sm:self-start">
          {showCase && (
            <Button
              type="button"
              variant={condition.caseInsensitive ? 'secondary' : 'ghost'}
              size="icon"
              className={cn(
                'size-10 font-mono text-xs',
                !condition.caseInsensitive && 'text-muted-foreground'
              )}
              aria-pressed={!!condition.caseInsensitive}
              aria-label="Ignore case"
              title={
                condition.caseInsensitive
                  ? 'Ignoring case'
                  : 'Case-sensitive. Click to ignore case'
              }
              onClick={() =>
                set((c) => ({ ...c, caseInsensitive: !c.caseInsensitive }))
              }
            >
              Aa
            </Button>
          )}
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="size-10 text-muted-foreground"
            aria-label={`Remove condition on ${meta.label.toLowerCase()}`}
            onClick={() => onRootChange(removeNode(root, id))}
          >
            <Trash2 />
          </Button>
        </div>
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
